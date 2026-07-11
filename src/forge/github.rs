//! GitHub implementation of the [`Forge`] trait, backed by octocrab.

mod api;
mod detect;
mod links;

use anyhow::Result;

use super::{
    Forge, IssueComment, PrContext, ReleasePlan, RepoRef, UpsertAction, block_on, parse_repo_url,
};
use api::{all_issue_comments, build_client, find_draft_release, upload_assets};
use detect::{is_github_dot_com, numeric_id, pr_context_from_event, repo_from_env};

pub(crate) use links::release_url;

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

    /// `None` for the default `api.github.com`, `Some(..)` for a GitHub Enterprise endpoint; an explicit `GITHUB_API_URL` wins.
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
            let existing = all_issue_comments(&client, repo, number)
                .await?
                .iter()
                .find(|c| c.body.as_deref().is_some_and(|b| b.contains(marker)))
                .map(|c| c.id);

            if let Some(id) = existing {
                // octocrab 0.54's `issues.update_comment` POSTs to the comment route, but GitHub only accepts PATCH there (POST 404s).
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

    /// Assets are attached to a draft that is then published, so the first publish
    /// is atomic and works with immutable releases. Re-runs recover an orphaned
    /// draft (which `get by tag` can't see) instead of duplicating it.
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
                let action = match releases.get_by_tag(&plan.tag).await {
                    // An immutable release is locked and can't be modified; leave it as-is.
                    Ok(existing) if existing.immutable == Some(true) => UpsertAction::Skipped,
                    Ok(existing) => {
                        let release = releases
                            .update(existing.id.0)
                            .name(plan.name.as_str())
                            .body(plan.body.as_str())
                            .draft(plan.draft)
                            .prerelease(plan.prerelease)
                            .send()
                            .await?;
                        upload_assets(&client, repo, &release, &plan.assets).await?;
                        UpsertAction::Updated
                    }
                    Err(octocrab::Error::GitHub { source, .. })
                        if source.status_code.as_u16() == 404 =>
                    {
                        // Create as a draft when there are assets, since immutable published releases reject uploads.
                        let create_as_draft = plan.draft || !plan.assets.is_empty();
                        // `get_by_tag` doesn't see drafts, so recover a draft from a crashed prior run instead of duplicating it.
                        let existing_draft = if create_as_draft {
                            find_draft_release(&client, repo, &plan.tag).await?
                        } else {
                            None
                        };
                        let release = match existing_draft {
                            Some(draft) => draft,
                            None => {
                                releases
                                    .create(plan.tag.as_str())
                                    .name(plan.name.as_str())
                                    .body(plan.body.as_str())
                                    .draft(create_as_draft)
                                    .prerelease(plan.prerelease)
                                    .send()
                                    .await?
                            }
                        };
                        upload_assets(&client, repo, &release, &plan.assets).await?;
                        if create_as_draft && !plan.draft {
                            releases.update(release.id.0).draft(false).send().await?;
                        }
                        UpsertAction::Created
                    }
                    Err(e) => {
                        return Err(anyhow::anyhow!(
                            "failed to look up GitHub release '{}': {}",
                            plan.tag,
                            e
                        ));
                    }
                };
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
                    if all_issue_comments(&client, repo, number)
                        .await?
                        .iter()
                        .any(|c| c.body.as_deref().is_some_and(|b| b.contains(marker)))
                    {
                        return Ok(false);
                    }
                    issues
                        .create_comment(number, format!("{}\n{}", marker, item.body))
                        .await?;
                    // Labels are best-effort: GitHub 422s on an undefined label, but the comment already landed, so just warn.
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
}
