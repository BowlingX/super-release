//! Async octocrab plumbing: client construction and the low-level release/comment API calls the [`Forge`](crate::forge::Forge) impl composes.

use anyhow::Result;
use octocrab::Octocrab;

use crate::forge::{RepoRef, ensure_crypto_provider};

/// Build an authenticated client; build it and issue all requests inside the same `block_on`, since octocrab spawns a worker at build time and must not be used across runtimes.
pub(super) async fn build_client(token: &str, base_uri: Option<&str>) -> Result<Octocrab> {
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

/// Find an orphaned draft release for `tag` left by a crashed prior run, scanning recent releases because `get_by_tag` only returns published ones.
pub(super) async fn find_draft_release(
    client: &Octocrab,
    repo: &RepoRef,
    tag: &str,
) -> Result<Option<octocrab::models::repos::Release>> {
    let page = client
        .repos(&repo.owner, &repo.repo)
        .releases()
        .list()
        .per_page(100)
        .send()
        .await?;
    Ok(page
        .items
        .into_iter()
        .find(|r| r.draft && r.tag_name == tag))
}

/// Upload each asset, replacing any existing same-named one for idempotent
/// re-runs. The release must be mutable (a draft, or on a repo without immutable
/// releases).
pub(super) async fn upload_assets(
    client: &Octocrab,
    repo: &RepoRef,
    release: &octocrab::models::repos::Release,
    assets: &[std::path::PathBuf],
) -> Result<()> {
    if assets.is_empty() {
        return Ok(());
    }
    let repos = client.repos(&repo.owner, &repo.repo);
    let releases = repos.releases();
    for path in assets {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| anyhow::anyhow!("invalid asset path: {}", path.display()))?;
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
    Ok(())
}

/// Fetch all comments on an issue/PR (following pagination).
pub(super) async fn all_issue_comments(
    client: &Octocrab,
    repo: &RepoRef,
    number: u64,
) -> Result<Vec<octocrab::models::issues::Comment>> {
    let first = client
        .issues(&repo.owner, &repo.repo)
        .list_comments(number)
        .per_page(100)
        .send()
        .await?;
    Ok(client.all_pages(first).await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::block_on;

    /// Guards the build-time footguns (crypto provider and runtime context) the offline `--no-comment` tests miss, building as production does inside `block_on`.
    #[test]
    fn client_builds_without_panicking() {
        block_on(async {
            assert!(build_client("dummy-token", None).await.is_ok());
        });
    }
}
