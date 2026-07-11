//! GitHub integration. The codebase is synchronous and octocrab is async, so
//! GitHub calls are bridged at the boundary with [`block_on`].

use anyhow::Result;
use octocrab::Octocrab;

/// A GitHub repository slug plus its host (for GitHub Enterprise).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubRepo {
    pub owner: String,
    pub repo: String,
    pub host: String,
}

impl GitHubRepo {
    fn is_github_dot_com(&self) -> bool {
        self.host.is_empty() || self.host.eq_ignore_ascii_case("github.com")
    }

    /// The API base URI octocrab should use: `None` for the default
    /// `api.github.com`, `Some(..)` for a GitHub Enterprise endpoint. An
    /// explicit `GITHUB_API_URL` wins.
    pub fn api_base_uri(&self) -> Option<String> {
        if let Ok(url) = std::env::var("GITHUB_API_URL") {
            let url = url.trim().trim_end_matches('/');
            if !url.is_empty() && url != "https://api.github.com" {
                return Some(url.to_string());
            }
        }
        if self.is_github_dot_com() {
            None
        } else {
            Some(format!("https://{}/api/v3", self.host))
        }
    }
}

/// Parse an owner/repo (and host) out of a git remote URL. Handles the common
/// forms: `git@host:owner/repo(.git)`, `ssh://git@host[:port]/owner/repo(.git)`,
/// and `https://host/owner/repo(.git)`, with optional trailing slash.
pub fn parse_repo_url(url: &str) -> Option<GitHubRepo> {
    let url = url.trim();
    let url = url.strip_suffix('/').unwrap_or(url);

    let (host_part, path) = if let Some((_scheme, after)) = url.split_once("://") {
        after.split_once('/')?
    } else if let Some((left, right)) = url.split_once(':') {
        // scp-like: [user@]host:owner/repo
        (left, right)
    } else {
        return None;
    };

    // Strip a leading `user@` and any `:port`.
    let host = host_part.rsplit('@').next().unwrap_or(host_part);
    let host = host.split(':').next().unwrap_or(host);

    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut segments = path.split('/').filter(|s| !s.is_empty());
    let owner = segments.next()?.to_string();
    let repo = segments.next()?.to_string();
    if host.is_empty() || owner.is_empty() || repo.is_empty() {
        return None;
    }

    Some(GitHubRepo {
        owner,
        repo,
        host: host.to_string(),
    })
}

/// Resolve the GitHub repo from the configured remote, falling back to the
/// `GITHUB_REPOSITORY` environment variable (set by GitHub Actions).
pub fn detect_repo(repo: &git2::Repository, remote_name: &str) -> Result<GitHubRepo> {
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

/// Build a [`GitHubRepo`] from the `GITHUB_REPOSITORY` (`owner/repo`) and
/// `GITHUB_SERVER_URL` environment variables provided by GitHub Actions.
pub fn repo_from_env() -> Option<GitHubRepo> {
    let slug = std::env::var("GITHUB_REPOSITORY").ok()?;
    let (owner, repo) = slug.split_once('/')?;
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    let host = std::env::var("GITHUB_SERVER_URL")
        .ok()
        .and_then(|u| parse_repo_url(&format!("{}/o/r", u.trim_end_matches('/'))).map(|g| g.host))
        .unwrap_or_else(|| "github.com".to_string());
    Some(GitHubRepo {
        owner: owner.to_string(),
        repo: repo.to_string(),
        host,
    })
}

/// The GitHub token from the environment: `GITHUB_TOKEN`, then `GH_TOKEN`.
pub fn token() -> Option<String> {
    for key in ["GITHUB_TOKEN", "GH_TOKEN"] {
        if let Ok(value) = std::env::var(key)
            && !value.trim().is_empty()
        {
            return Some(value);
        }
    }
    None
}

/// octocrab's connector uses the process-default rustls provider, which rustls
/// can't auto-select when both `ring` and `aws-lc-rs` are in the tree (they
/// are, transitively) — it panics. Install `ring` once; a no-op if some other
/// component already installed one.
fn ensure_crypto_provider() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
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

/// A pull-request context discovered from the GitHub Actions environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrContext {
    pub number: u64,
    pub base_ref: Option<String>,
}

/// Detect the current pull-request context from the GitHub Actions environment
/// (`GITHUB_EVENT_PATH` payload, falling back to `GITHUB_REF`).
pub fn detect_pr_context() -> Option<PrContext> {
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
        && let Ok(number) = num.parse::<u64>()
    {
        let base_ref = std::env::var("GITHUB_BASE_REF")
            .ok()
            .filter(|s| !s.is_empty());
        return Some(PrContext { number, base_ref });
    }

    None
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
        return Some(PrContext { number, base_ref });
    }
    // e.g. `issue_comment` events carry the number at the top level.
    if let Some(number) = json.get("number").and_then(serde_json::Value::as_u64) {
        return Some(PrContext {
            number,
            base_ref: None,
        });
    }
    None
}

/// Whether an upsert created a new resource or updated an existing one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpsertAction {
    Created,
    Updated,
}

impl UpsertAction {
    pub fn verb(self) -> &'static str {
        match self {
            UpsertAction::Created => "Created",
            UpsertAction::Updated => "Updated",
        }
    }
}

/// Create, or update in place, a "sticky" issue/PR comment: the first existing
/// comment whose body contains `marker` is updated, otherwise a new one is
/// created, so repeated preview runs replace the same comment.
pub fn upsert_issue_comment(
    token: &str,
    base_uri: Option<&str>,
    repo: &GitHubRepo,
    issue_number: u64,
    marker: &str,
    body: &str,
) -> Result<UpsertAction> {
    block_on(async move {
        let client = build_client(token, base_uri).await?;
        let issues = client.issues(&repo.owner, &repo.repo);
        let first = issues
            .list_comments(issue_number)
            .per_page(100)
            .send()
            .await?;
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
            issues.create_comment(issue_number, body).await?;
            Ok(UpsertAction::Created)
        }
    })
}

/// A single GitHub Release to create or update, and the assets to attach.
pub struct ReleasePlan {
    pub tag: String,
    pub name: String,
    pub body: String,
    pub draft: bool,
    pub prerelease: bool,
    pub assets: Vec<std::path::PathBuf>,
}

/// Create or update (idempotent by tag) a GitHub Release per plan, replacing any
/// existing same-named assets. All requests share one client/runtime. Returns
/// each release's tag and whether it was created or updated.
///
/// Idempotency uses "get release by tag", which GitHub only returns for
/// *published* releases — so re-running with `draft: true` won't find the prior
/// draft and will fail to re-create it. Non-draft releases re-run cleanly.
pub fn publish_releases(
    token: &str,
    base_uri: Option<&str>,
    repo: &GitHubRepo,
    plans: &[ReleasePlan],
) -> Result<Vec<(String, UpsertAction)>> {
    block_on(async move {
        let client = build_client(token, base_uri).await?;
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
                // Replace a same-named asset from a previous run so re-runs are idempotent.
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

/// Run a future to completion on a fresh current-thread runtime. This is the
/// async→sync bridge that keeps the rest of the tool synchronous.
pub fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime")
        .block_on(fut)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(owner: &str, name: &str, host: &str) -> GitHubRepo {
        GitHubRepo {
            owner: owner.into(),
            repo: name.into(),
            host: host.into(),
        }
    }

    #[test]
    fn parses_https_url() {
        assert_eq!(
            parse_repo_url("https://github.com/BowlingX/super-release.git"),
            Some(repo("BowlingX", "super-release", "github.com"))
        );
        assert_eq!(
            parse_repo_url("https://github.com/BowlingX/super-release"),
            Some(repo("BowlingX", "super-release", "github.com"))
        );
        assert_eq!(
            parse_repo_url("https://github.com/BowlingX/super-release/"),
            Some(repo("BowlingX", "super-release", "github.com"))
        );
    }

    #[test]
    fn parses_scp_ssh_url() {
        assert_eq!(
            parse_repo_url("git@github.com:BowlingX/super-release.git"),
            Some(repo("BowlingX", "super-release", "github.com"))
        );
        assert_eq!(
            parse_repo_url("ssh://git@github.com/BowlingX/super-release.git"),
            Some(repo("BowlingX", "super-release", "github.com"))
        );
        assert_eq!(
            parse_repo_url("ssh://git@github.com:22/BowlingX/super-release.git"),
            Some(repo("BowlingX", "super-release", "github.com"))
        );
    }

    #[test]
    fn parses_enterprise_host() {
        assert_eq!(
            parse_repo_url("git@github.example.com:acme/widgets.git"),
            Some(repo("acme", "widgets", "github.example.com"))
        );
        assert_eq!(
            parse_repo_url("https://github.example.com/acme/widgets"),
            Some(repo("acme", "widgets", "github.example.com"))
        );
    }

    #[test]
    fn rejects_non_repo_urls() {
        assert_eq!(parse_repo_url("not a url"), None);
        assert_eq!(parse_repo_url("https://github.com/only-owner"), None);
    }

    #[test]
    fn enterprise_api_base_uri() {
        assert_eq!(repo("a", "b", "github.com").api_base_uri(), None);
        // Note: this reads GITHUB_API_URL; in a clean test env it is unset.
        assert_eq!(
            repo("a", "b", "ghe.corp").api_base_uri(),
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
                number: 42,
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
                number: 7,
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
