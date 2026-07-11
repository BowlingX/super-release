//! GitHub implementation of the [`Forge`] trait, backed by octocrab.

use anyhow::Result;
use octocrab::Octocrab;

use super::{
    Forge, IssueComment, PrContext, ReleasePlan, RepoRef, UpsertAction, block_on,
    ensure_crypto_provider, parse_repo_url,
};

pub struct GitHubForge;

impl Forge for GitHubForge {
    fn token(&self) -> Option<String> {
        for key in ["GITHUB_TOKEN", "GH_TOKEN"] {
            if let Ok(value) = std::env::var(key)
                && !value.trim().is_empty()
            {
                return Some(value);
            }
        }
        None
    }

    fn detect_repo(&self, repo: &git2::Repository, remote_name: &str) -> Result<RepoRef> {
        if let Ok(remote) = repo.find_remote(remote_name)
            && let Some(url) = remote.url()
            && let Some(parsed) = parse_repo_url(url)
        {
            return Ok(parsed);
        }
        if let Some(from_env) = repo_from_env() {
            return Ok(from_env);
        }
        anyhow::bail!(
            "Could not determine the GitHub owner/repo from remote '{}' or the \
             GITHUB_REPOSITORY environment variable",
            remote_name
        )
    }

    fn detect_pr_context(&self) -> Option<PrContext> {
        if let Ok(path) = std::env::var("GITHUB_EVENT_PATH")
            && let Ok(content) = std::fs::read_to_string(&path)
            && let Ok(json) = serde_json::from_str::<serde_json::Value>(&content)
            && let Some(ctx) = pr_context_from_event(&json)
        {
            return Some(ctx);
        }

        // Fallback: `refs/pull/<n>/merge`.
        if let Ok(gh_ref) = std::env::var("GITHUB_REF")
            && let Some(rest) = gh_ref.strip_prefix("refs/pull/")
            && let Some((num, _)) = rest.split_once('/')
            && num.parse::<u64>().is_ok()
        {
            let base_ref = std::env::var("GITHUB_BASE_REF")
                .ok()
                .filter(|s| !s.is_empty());
            return Some(PrContext {
                id: num.to_string(),
                base_ref,
            });
        }

        None
    }

    /// `None` for the default `api.github.com`, `Some(..)` for a GitHub
    /// Enterprise endpoint. An explicit `GITHUB_API_URL` wins.
    fn api_base_uri(&self, repo: &RepoRef) -> Option<String> {
        if let Ok(url) = std::env::var("GITHUB_API_URL") {
            let url = url.trim().trim_end_matches('/');
            if !url.is_empty() && url != "https://api.github.com" {
                return Some(url.to_string());
            }
        }
        if is_github_dot_com(&repo.host) {
            None
        } else {
            Some(format!("https://{}/api/v3", repo.host))
        }
    }

    fn upsert_pr_comment(
        &self,
        token: &str,
        api_url: Option<&str>,
        repo: &RepoRef,
        id: &str,
        marker: &str,
        body: &str,
    ) -> Result<UpsertAction> {
        let number = numeric_id(id)?;
        block_on(async move {
            let client = build_client(token, api_url).await?;
            let issues = client.issues(&repo.owner, &repo.repo);
            let first = issues.list_comments(number).per_page(100).send().await?;
            let comments = client.all_pages(first).await?;
            let existing = comments
                .iter()
                .find(|c| c.body.as_deref().is_some_and(|b| b.contains(marker)))
                .map(|c| c.id);

            if let Some(id) = existing {
                // NOTE: octocrab 0.54's `issues.update_comment` issues an HTTP POST
                // to the comment route, but GitHub only accepts PATCH there (POST
                // 404s). Issue the PATCH directly so updates actually land.
                let route = format!("/repos/{}/{}/issues/comments/{}", repo.owner, repo.repo, id);
                let _updated: octocrab::models::issues::Comment = client
                    .patch(route, Some(&serde_json::json!({ "body": body })))
                    .await?;
                Ok(UpsertAction::Updated)
            } else {
                issues.create_comment(number, body).await?;
                Ok(UpsertAction::Created)
            }
        })
    }

    /// Idempotency uses "get release by tag", which GitHub only returns for
    /// *published* releases — so re-running with `draft: true` won't find the
    /// prior draft and will fail to re-create it. Non-draft releases re-run
    /// cleanly.
    fn publish_releases(
        &self,
        token: &str,
        api_url: Option<&str>,
        repo: &RepoRef,
        plans: &[ReleasePlan],
    ) -> Result<Vec<(String, UpsertAction)>> {
        block_on(async move {
            let client = build_client(token, api_url).await?;
            let repos = client.repos(&repo.owner, &repo.repo);
            let releases = repos.releases();
            let mut results = Vec::with_capacity(plans.len());

            for plan in plans {
                let (release, action) = match releases.get_by_tag(&plan.tag).await {
                    Ok(existing) => {
                        let updated = releases
                            .update(existing.id.0)
                            .name(plan.name.as_str())
                            .body(plan.body.as_str())
                            .draft(plan.draft)
                            .prerelease(plan.prerelease)
                            .send()
                            .await?;
                        (updated, UpsertAction::Updated)
                    }
                    Err(octocrab::Error::GitHub { source, .. })
                        if source.status_code.as_u16() == 404 =>
                    {
                        let created = releases
                            .create(plan.tag.as_str())
                            .name(plan.name.as_str())
                            .body(plan.body.as_str())
                            .draft(plan.draft)
                            .prerelease(plan.prerelease)
                            .send()
                            .await?;
                        (created, UpsertAction::Created)
                    }
                    Err(e) => {
                        return Err(anyhow::anyhow!(
                            "failed to look up GitHub release '{}': {}",
                            plan.tag,
                            e
                        ));
                    }
                };

                for path in &plan.assets {
                    let name = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .ok_or_else(|| anyhow::anyhow!("invalid asset path: {}", path.display()))?;
                    // Replace a same-named asset from a previous run so re-runs
                    // are idempotent.
                    if let Some(existing) = release.assets.iter().find(|a| a.name == name) {
                        repos.release_assets().delete(existing.id.0).await?;
                    }
                    let data = std::fs::read(path)
                        .map_err(|e| anyhow::anyhow!("reading asset {}: {}", path.display(), e))?;
                    releases
                        .upload_asset(release.id.0, name, bytes::Bytes::from(data))
                        .send()
                        .await?;
                }

                results.push((plan.tag.clone(), action));
            }

            Ok(results)
        })
    }

    fn comment_on_issues(
        &self,
        token: &str,
        api_url: Option<&str>,
        repo: &RepoRef,
        marker: &str,
        items: &[IssueComment],
    ) -> Result<usize> {
        block_on(async move {
            let client = build_client(token, api_url).await?;
            let issues = client.issues(&repo.owner, &repo.repo);
            let mut count = 0;

            for item in items {
                let posted: Result<bool> = async {
                    let number = numeric_id(&item.id)?;
                    let first = issues.list_comments(number).per_page(100).send().await?;
                    let comments = client.all_pages(first).await?;
                    if comments
                        .iter()
                        .any(|c| c.body.as_deref().is_some_and(|b| b.contains(marker)))
                    {
                        return Ok(false);
                    }
                    issues
                        .create_comment(number, format!("{}\n{}", marker, item.body))
                        .await?;
                    // Labels are best-effort: GitHub 422s on a label the repo
                    // hasn't defined, but the comment already landed — don't fail
                    // the item (which would drop the count and mislead), just warn.
                    if !item.labels.is_empty()
                        && let Err(e) = issues.add_labels(number, &item.labels).await
                    {
                        eprintln!(
                            "  [github] Warning: commented on #{} but could not add labels: {}",
                            item.id, e
                        );
                    }
                    Ok(true)
                }
                .await;

                match posted {
                    Ok(true) => count += 1,
                    Ok(false) => {}
                    Err(e) => eprintln!(
                        "  [github] Warning: could not comment on #{}: {}",
                        item.id, e
                    ),
                }
            }

            Ok(count)
        })
    }
}

/// GitHub identifies issues/PRs by number; parse the neutral string id.
fn numeric_id(id: &str) -> Result<u64> {
    id.parse::<u64>()
        .map_err(|_| anyhow::anyhow!("GitHub issue/PR id must be numeric, got '{}'", id))
}

fn is_github_dot_com(host: &str) -> bool {
    host.is_empty() || host.eq_ignore_ascii_case("github.com")
}

/// Build a [`RepoRef`] from the `GITHUB_REPOSITORY` (`owner/repo`) and
/// `GITHUB_SERVER_URL` environment variables provided by GitHub Actions.
fn repo_from_env() -> Option<RepoRef> {
    let slug = std::env::var("GITHUB_REPOSITORY").ok()?;
    let (owner, repo) = slug.split_once('/')?;
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    let host = std::env::var("GITHUB_SERVER_URL")
        .ok()
        .and_then(|u| parse_repo_url(&format!("{}/o/r", u.trim_end_matches('/'))).map(|g| g.host))
        .unwrap_or_else(|| "github.com".to_string());
    Some(RepoRef {
        owner: owner.to_string(),
        repo: repo.to_string(),
        host,
    })
}

/// Pure extraction of a [`PrContext`] from a parsed webhook event payload.
fn pr_context_from_event(json: &serde_json::Value) -> Option<PrContext> {
    if let Some(pr) = json.get("pull_request")
        && let Some(number) = pr.get("number").and_then(serde_json::Value::as_u64)
    {
        let base_ref = pr
            .get("base")
            .and_then(|b| b.get("ref"))
            .and_then(serde_json::Value::as_str)
            .map(String::from);
        return Some(PrContext {
            id: number.to_string(),
            base_ref,
        });
    }
    // e.g. `issue_comment` events carry the number at the top level.
    if let Some(number) = json.get("number").and_then(serde_json::Value::as_u64) {
        return Some(PrContext {
            id: number.to_string(),
            base_ref: None,
        });
    }
    None
}

/// Build an authenticated client. Must run inside a tokio runtime: octocrab's
/// retry/timeout layers spawn a worker at build time, so build the client and
/// issue all requests inside the same [`block_on`] — never across runtimes.
async fn build_client(token: &str, base_uri: Option<&str>) -> Result<Octocrab> {
    ensure_crypto_provider();
    let mut builder = Octocrab::builder().personal_token(token.to_string());
    if let Some(base) = base_uri {
        builder = builder
            .base_uri(base)
            .map_err(|e| anyhow::anyhow!("invalid GitHub API base URI '{}': {}", base, e))?;
    }
    builder
        .build()
        .map_err(|e| anyhow::anyhow!("failed to build GitHub client: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(host: &str) -> RepoRef {
        RepoRef {
            owner: "a".into(),
            repo: "b".into(),
            host: host.into(),
        }
    }

    #[test]
    fn enterprise_api_base_uri() {
        assert_eq!(GitHubForge.api_base_uri(&repo("github.com")), None);
        // Note: this reads GITHUB_API_URL; in a clean test env it is unset.
        assert_eq!(
            GitHubForge.api_base_uri(&repo("ghe.corp")),
            Some("https://ghe.corp/api/v3".to_string())
        );
    }

    #[test]
    fn pr_context_from_pull_request_event() {
        let json = serde_json::json!({
            "pull_request": { "number": 42, "base": { "ref": "main" } }
        });
        assert_eq!(
            pr_context_from_event(&json),
            Some(PrContext {
                id: "42".into(),
                base_ref: Some("main".into())
            })
        );
    }

    #[test]
    fn pr_context_from_top_level_number() {
        let json = serde_json::json!({ "number": 7 });
        assert_eq!(
            pr_context_from_event(&json),
            Some(PrContext {
                id: "7".into(),
                base_ref: None
            })
        );
    }

    #[test]
    fn pr_context_absent() {
        assert_eq!(pr_context_from_event(&serde_json::json!({})), None);
    }

    /// Guards the two build-time footguns the offline `--no-comment` tests miss:
    /// the crypto provider and the runtime context. Builds it as production does
    /// (inside `block_on`, no DNS) so a regression fails here, not in CI.
    #[test]
    fn client_builds_without_panicking() {
        block_on(async {
            assert!(build_client("dummy-token", None).await.is_ok());
        });
    }
}
