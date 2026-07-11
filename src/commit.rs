use regex::Regex;
use std::fmt;
use std::sync::LazyLock;

static CONVENTIONAL_COMMIT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?P<type>[a-zA-Z]+)(?:\((?P<scope>[^)]+)\))?(?P<bang>!)?:\s*(?P<desc>.+)")
        .unwrap()
});

/// The `(#123)` pull-request suffix GitHub appends to a squash/merge subject.
static PR_SUFFIX_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\(#(\d+)\)\s*$").unwrap());

/// A classic `Merge pull request #123 from ...` merge-commit subject.
static MERGE_PR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^Merge pull request #(\d+)").unwrap());

/// GitHub issue-closing keywords, e.g. `fixes #12`, `Closes #34`.
static CLOSING_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:close[sd]?|fix(?:es|ed)?|resolve[sd]?)\s+#(\d+)").unwrap()
});

/// Deduplicated PR/issue references a commit resolves; plain `#123` mentions without a closing keyword are ignored to avoid commenting on unrelated issues.
pub fn referenced_issues(message: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |n: &str| {
        if !out.iter().any(|e| e == n) {
            out.push(n.to_string());
        }
    };

    let subject = message.lines().next().unwrap_or("");
    if let Some(caps) = PR_SUFFIX_RE.captures(subject) {
        push(&caps[1]);
    }
    if let Some(caps) = MERGE_PR_RE.captures(subject) {
        push(&caps[1]);
    }
    for caps in CLOSING_RE.captures_iter(message) {
        push(&caps[1]);
    }
    out
}

/// The type of version bump a commit implies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BumpLevel {
    None,
    Patch,
    Minor,
    Major,
}

impl fmt::Display for BumpLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BumpLevel::None => write!(f, "none"),
            BumpLevel::Patch => write!(f, "patch"),
            BumpLevel::Minor => write!(f, "minor"),
            BumpLevel::Major => write!(f, "major"),
        }
    }
}

/// A parsed conventional commit.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ConventionalCommit {
    /// Short hash for display (8 chars).
    pub hash: String,
    /// Full commit OID for exact comparisons.
    pub oid: Option<git2::Oid>,
    pub commit_type: String,
    pub scope: Option<String>,
    pub description: String,
    pub body: Option<String>,
    pub breaking: bool,
    pub bump: BumpLevel,
    pub raw_message: String,
    /// Files changed by this commit (relative paths).
    pub files_changed: Vec<String>,
}

/// Parse `<type>(<scope>)!: <description>`; `hash` and `files_changed` are set by the caller after parsing.
pub fn parse_conventional_commit(hash: &str, message: &str) -> Option<ConventionalCommit> {
    let first_line = message.lines().next()?;
    let caps = CONVENTIONAL_COMMIT_RE.captures(first_line)?;

    let commit_type = caps.name("type")?.as_str().to_lowercase();
    let scope = caps.name("scope").map(|m| m.as_str().to_string());
    let description = caps.name("desc")?.as_str().trim().to_string();
    let bang = caps.name("bang").is_some();

    let body = message
        .split_once("\n\n")
        .map(|(_, b)| b.trim().to_string())
        .filter(|b| !b.is_empty());

    let breaking =
        bang || message.contains("BREAKING CHANGE:") || message.contains("BREAKING-CHANGE:");

    let bump = if breaking {
        BumpLevel::Major
    } else {
        match commit_type.as_str() {
            "feat" => BumpLevel::Minor,
            "fix" | "perf" => BumpLevel::Patch,
            "revert" => BumpLevel::Patch,
            _ => BumpLevel::None,
        }
    };

    Some(ConventionalCommit {
        hash: hash.to_string(),
        oid: None,
        commit_type,
        scope,
        description,
        body,
        breaking,
        bump,
        raw_message: message.to_string(),
        files_changed: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_feat() {
        let c = parse_conventional_commit("abc123", "feat: add new feature").unwrap();
        assert_eq!(c.commit_type, "feat");
        assert_eq!(c.description, "add new feature");
        assert_eq!(c.bump, BumpLevel::Minor);
        assert!(!c.breaking);
        assert!(c.scope.is_none());
    }

    #[test]
    fn test_parse_fix_with_scope() {
        let c = parse_conventional_commit("abc123", "fix(parser): handle edge case").unwrap();
        assert_eq!(c.commit_type, "fix");
        assert_eq!(c.scope.as_deref(), Some("parser"));
        assert_eq!(c.bump, BumpLevel::Patch);
    }

    #[test]
    fn test_parse_breaking_bang() {
        let c = parse_conventional_commit("abc123", "feat!: remove old API").unwrap();
        assert!(c.breaking);
        assert_eq!(c.bump, BumpLevel::Major);
    }

    #[test]
    fn test_parse_breaking_footer() {
        let msg = "feat: new thing\n\nBREAKING CHANGE: old thing removed";
        let c = parse_conventional_commit("abc123", msg).unwrap();
        assert!(c.breaking);
        assert_eq!(c.bump, BumpLevel::Major);
        assert!(c.body.is_some());
    }

    #[test]
    fn test_parse_chore() {
        let c = parse_conventional_commit("abc123", "chore: update deps").unwrap();
        assert_eq!(c.commit_type, "chore");
        assert_eq!(c.bump, BumpLevel::None);
    }

    #[test]
    fn test_non_conventional_returns_none() {
        assert!(parse_conventional_commit("abc123", "just a random message").is_none());
        assert!(parse_conventional_commit("abc123", "").is_none());
    }

    #[test]
    fn test_parse_perf() {
        let c = parse_conventional_commit("abc123", "perf(core): optimize loop").unwrap();
        assert_eq!(c.bump, BumpLevel::Patch);
    }

    #[test]
    fn test_no_bump_commit_types() {
        for msg in &[
            "chore: update deps",
            "docs: update readme",
            "style: format code",
            "test: add unit tests",
            "ci: update workflow",
            "build: update config",
            "refactor: simplify logic",
        ] {
            let c = parse_conventional_commit("abc123", msg).unwrap();
            assert_eq!(c.bump, BumpLevel::None, "Expected no bump for: {}", msg);
        }
    }

    #[test]
    fn test_bump_commit_types() {
        let cases = [
            ("feat: add feature", BumpLevel::Minor),
            ("fix: fix bug", BumpLevel::Patch),
            ("perf: optimize", BumpLevel::Patch),
            ("revert: undo thing", BumpLevel::Patch),
            ("feat!: breaking", BumpLevel::Major),
            ("fix!: breaking fix", BumpLevel::Major),
            ("chore!: breaking chore", BumpLevel::Major),
        ];
        for (msg, expected) in &cases {
            let c = parse_conventional_commit("abc123", msg).unwrap();
            assert_eq!(c.bump, *expected, "Wrong bump for: {}", msg);
        }
    }

    #[test]
    fn test_breaking_change_footer_on_any_type() {
        for msg in &[
            "chore: thing\n\nBREAKING CHANGE: breaks stuff",
            "docs: update\n\nBREAKING-CHANGE: api changed",
            "refactor: rewrite\n\nBREAKING CHANGE: new interface",
        ] {
            let c = parse_conventional_commit("abc123", msg).unwrap();
            assert!(c.breaking, "Should be breaking: {}", msg);
            assert_eq!(c.bump, BumpLevel::Major, "Should be major: {}", msg);
        }
    }

    #[test]
    fn test_body_extraction() {
        let msg = "feat: something\n\nThis is the body\nwith multiple lines";
        let c = parse_conventional_commit("abc123", msg).unwrap();
        assert_eq!(
            c.body.as_deref(),
            Some("This is the body\nwith multiple lines")
        );
    }

    #[test]
    fn test_referenced_issues_squash_suffix() {
        assert_eq!(referenced_issues("feat: add thing (#123)"), vec!["123"]);
    }

    #[test]
    fn test_referenced_issues_merge_commit() {
        assert_eq!(
            referenced_issues("Merge pull request #45 from foo/bar"),
            vec!["45"]
        );
    }

    #[test]
    fn test_referenced_issues_closing_keywords() {
        let msg = "fix: bug (#10)\n\nCloses #20, fixes #21\nresolved #22";
        assert_eq!(referenced_issues(msg), vec!["10", "20", "21", "22"]);
    }

    #[test]
    fn test_referenced_issues_ignores_plain_mentions() {
        // `#99` is a bare mention, not a closing keyword → ignored.
        assert_eq!(
            referenced_issues("fix: handle #99 edge case"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn test_referenced_issues_dedupes() {
        assert_eq!(referenced_issues("feat: x (#7)\n\nfixes #7"), vec!["7"]);
    }
}
