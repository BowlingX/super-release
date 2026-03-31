use regex::Regex;
use std::fmt;
use std::sync::LazyLock;

static CONVENTIONAL_COMMIT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?P<type>[a-zA-Z]+)(?:\((?P<scope>[^)]+)\))?(?P<bang>!)?:\s*(?P<desc>.+)")
        .unwrap()
});

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
    pub hash: String,
    pub commit_type: String,
    pub scope: Option<String>,
    pub description: String,
    pub body: Option<String>,
    pub breaking: bool,
    pub bump: BumpLevel,
    /// The full original commit message (needed by git-cliff).
    pub raw_message: String,
    /// Files changed by this commit (relative paths).
    pub files_changed: Vec<String>,
}

/// Parse a conventional commit message into its components.
/// Format: <type>(<scope>)!: <description>
///
/// The `hash` and `files_changed` are set by the caller after parsing.
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

    let breaking = bang
        || message.contains("BREAKING CHANGE:")
        || message.contains("BREAKING-CHANGE:");

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
        assert_eq!(c.body.as_deref(), Some("This is the body\nwith multiple lines"));
    }
}
