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
            comment_id: None,
        });
    }
    // `issue_comment` fires for issues and PRs alike; only a PR's issue carries
    // `pull_request`. The base ref isn't in the payload.
    if let Some(issue) = json.get("issue")
        && issue.get("pull_request").is_some()
        && let Some(number) = issue.get("number").and_then(serde_json::Value::as_u64)
    {
        return Some(PrContext {
            id: number.to_string(),
            base_ref: None,
            comment_id: json
                .get("comment")
                .and_then(|c| c.get("id"))
                .and_then(serde_json::Value::as_u64),
        });
    }
    // Other PR events without a `pull_request` object carry the number at the top level.
    if let Some(number) = json.get("number").and_then(serde_json::Value::as_u64) {
        return Some(PrContext {
            id: number.to_string(),
            base_ref: None,
            comment_id: None,
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
                base_ref: Some("main".into()),
                comment_id: None,
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
                base_ref: None,
                comment_id: None,
            })
        );
    }

    #[test]
    fn pr_context_from_pr_comment_event() {
        let json = serde_json::json!({
            "action": "created",
            "issue": { "number": 12, "pull_request": { "url": "https://api.github.com/x" } },
            "comment": { "id": 99, "body": "@super-release release-notes" }
        });
        assert_eq!(
            pr_context_from_event(&json),
            Some(PrContext {
                id: "12".into(),
                base_ref: None,
                comment_id: Some(99),
            })
        );
    }

    /// A comment on a plain issue isn't a PR context.
    #[test]
    fn issue_comment_on_an_issue_is_not_a_pr() {
        let json = serde_json::json!({
            "issue": { "number": 12 },
            "comment": { "id": 99 }
        });
        assert_eq!(pr_context_from_event(&json), None);
    }

    #[test]
    fn pr_context_absent() {
        assert_eq!(pr_context_from_event(&serde_json::json!({})), None);
    }
}
