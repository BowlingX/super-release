//! Pure parsing/classification helpers: reading the repo and PR context from the
//! GitHub Actions environment, host classification, and id validation. No network.

use anyhow::Result;

use crate::forge::{PrContext, RepoRef, parse_repo_url};

/// GitHub identifies issues/PRs by number; parse the neutral string id.
pub(super) fn numeric_id(id: &str) -> Result<u64> {
    id.parse::<u64>()
        .map_err(|_| anyhow::anyhow!("GitHub issue/PR id must be numeric, got '{}'", id))
}

pub(super) fn is_github_dot_com(host: &str) -> bool {
    host.is_empty() || host.eq_ignore_ascii_case("github.com")
}

/// Build a [`RepoRef`] from the `GITHUB_REPOSITORY` (`owner/repo`) and
/// `GITHUB_SERVER_URL` environment variables provided by GitHub Actions.
pub(super) fn repo_from_env() -> Option<RepoRef> {
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

pub(super) fn pr_context_from_event(json: &serde_json::Value) -> Option<PrContext> {
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
