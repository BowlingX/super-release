//! Provider-neutral "forge" abstraction.
//!
//! Each git host implements [`Forge`]; sync code bridges to async provider
//! clients at the boundary with [`block_on`].

pub mod github;

use anyhow::Result;

/// A repository slug plus the host it lives on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRef {
    pub owner: String,
    pub repo: String,
    pub host: String,
}

impl RepoRef {
    /// The repository's web (browser) URL, e.g. `https://github.com/owner/repo`.
    pub fn web_url(&self) -> String {
        format!("https://{}/{}/{}", self.host, self.owner, self.repo)
    }
}

/// A pull/merge-request context from CI; the id is a string to stay provider-neutral
/// (numeric on git hosts, keys like `PROJ-123` on issue trackers such as JIRA).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrContext {
    pub id: String,
    pub base_ref: Option<String>,
}

/// The outcome of an upsert: a new resource created, an existing one updated, or
/// left untouched because it was already in the desired (or an unmodifiable) state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpsertAction {
    Created,
    Updated,
    Skipped,
}

impl UpsertAction {
    pub fn verb(self) -> &'static str {
        match self {
            UpsertAction::Created => "Created",
            UpsertAction::Updated => "Updated",
            UpsertAction::Skipped => "Skipped",
        }
    }
}

/// A release to create or update with its assets; providers without a first-class
/// release object (e.g. Bitbucket) may map this to a tag/downloads.
pub struct ReleasePlan {
    pub tag: String,
    pub name: String,
    pub body: String,
    pub draft: bool,
    pub prerelease: bool,
    pub assets: Vec<std::path::PathBuf>,
}

/// A "released" comment (and labels) to post on a resolved issue or PR; the id is
/// a string to stay provider-neutral (keys like `PROJ-123` on issue trackers).
pub struct IssueComment {
    pub id: String,
    /// Rendered comment body (the marker is added by the provider).
    pub body: String,
    pub labels: Vec<String>,
}

/// A git host provider; write operations build their client internally from the
/// token and optional API base URI so the call stays on one runtime (see [`block_on`]).
pub trait Forge: Send + Sync {
    /// The API token from the environment, if present.
    fn token(&self) -> Option<String>;

    /// Resolve owner/repo/host from the git remote or CI environment.
    fn detect_repo(&self, repo: &git2::Repository, remote_name: &str) -> Result<RepoRef>;

    /// Detect the current pull/merge-request from the CI environment.
    fn detect_pr_context(&self) -> Option<PrContext>;

    /// Effective API base URI for a repo (self-hosted/Enterprise), or `None` for
    /// the provider's default host.
    fn api_base_uri(&self, repo: &RepoRef) -> Option<String>;

    /// Post or update in place a sticky PR/MR comment (idempotent via `marker`).
    fn upsert_pr_comment(
        &self,
        token: &str,
        api_url: Option<&str>,
        repo: &RepoRef,
        id: &str,
        marker: &str,
        body: &str,
    ) -> Result<UpsertAction>;

    /// Create or update (idempotent) a release per plan. Returns each release's
    /// tag and whether it was created or updated.
    fn publish_releases(
        &self,
        token: &str,
        api_url: Option<&str>,
        repo: &RepoRef,
        plans: &[ReleasePlan],
    ) -> Result<Vec<(String, UpsertAction)>>;

    /// Comment on resolved issues/PRs and add labels, skipping any already
    /// carrying `marker`. Returns how many were newly commented on.
    fn comment_on_issues(
        &self,
        token: &str,
        api_url: Option<&str>,
        repo: &RepoRef,
        marker: &str,
        items: &[IssueComment],
    ) -> Result<usize>;
}

/// Pick the provider for a repository from its remote host. GitHub is the
/// default, which also covers GitHub Enterprise on arbitrary hosts.
#[allow(clippy::match_single_binding)] // dispatch stub: more arms land with more providers
pub fn resolve_forge(repo: &git2::Repository, remote_name: &str) -> Box<dyn Forge> {
    match remote_host(repo, remote_name).as_deref() {
        // Some(h) if h.contains("bitbucket") => Box::new(bitbucket::BitbucketForge),
        // Some(h) if h.contains("gitlab") => Box::new(gitlab::GitLabForge),
        _ => Box::new(github::GitHubForge),
    }
}

/// The host of the configured remote, if it parses as a repo URL.
fn remote_host(repo: &git2::Repository, remote_name: &str) -> Option<String> {
    let remote = repo.find_remote(remote_name).ok()?;
    parse_repo_url(remote.url()?).map(|r| r.host)
}

/// Parse an owner/repo (and host) out of a git remote URL. Handles the common
/// forms: `git@host:owner/repo(.git)`, `ssh://git@host[:port]/owner/repo(.git)`,
/// and `https://host/owner/repo(.git)`, with optional trailing slash.
pub fn parse_repo_url(url: &str) -> Option<RepoRef> {
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

    let host = host_part.rsplit('@').next().unwrap_or(host_part);
    let host = host.split(':').next().unwrap_or(host);

    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut segments = path.split('/').filter(|s| !s.is_empty());
    let owner = segments.next()?.to_string();
    let repo = segments.next()?.to_string();
    if host.is_empty() || owner.is_empty() || repo.is_empty() {
        return None;
    }

    Some(RepoRef {
        owner,
        repo,
        host: host.to_string(),
    })
}

/// Install `ring` as the rustls crypto provider once, since rustls can't auto-select
/// (and panics) when both `ring` and `aws-lc-rs` are in the tree, as they transitively are.
pub(crate) fn ensure_crypto_provider() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// Run a future to completion on a fresh current-thread runtime — the async→sync
/// bridge that keeps the rest of the tool synchronous.
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

    fn repo(owner: &str, name: &str, host: &str) -> RepoRef {
        RepoRef {
            owner: owner.into(),
            repo: name.into(),
            host: host.into(),
        }
    }

    #[test]
    fn web_url_is_host_owner_repo() {
        assert_eq!(
            repo("BowlingX", "super-release", "github.com").web_url(),
            "https://github.com/BowlingX/super-release"
        );
        assert_eq!(
            repo("acme", "widgets", "github.example.com").web_url(),
            "https://github.example.com/acme/widgets"
        );
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
    fn parses_enterprise_and_other_hosts() {
        assert_eq!(
            parse_repo_url("git@github.example.com:acme/widgets.git"),
            Some(repo("acme", "widgets", "github.example.com"))
        );
        assert_eq!(
            parse_repo_url("git@bitbucket.org:team/thing.git"),
            Some(repo("team", "thing", "bitbucket.org"))
        );
    }

    #[test]
    fn rejects_non_repo_urls() {
        assert_eq!(parse_repo_url("not a url"), None);
        assert_eq!(parse_repo_url("https://github.com/only-owner"), None);
    }
}
