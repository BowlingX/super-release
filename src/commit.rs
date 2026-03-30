use regex::Regex;
use std::fmt;

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
pub struct ConventionalCommit {
    pub hash: String,
    pub commit_type: String,
    pub scope: Option<String>,
    pub description: String,
    pub body: Option<String>,
    pub breaking: bool,
    pub bump: BumpLevel,
    /// Files changed by this commit (relative paths).
    pub files_changed: Vec<String>,
}

/// Parse a conventional commit message into its components.
/// Format: <type>(<scope>)!: <description>
///
/// The `hash` and `files_changed` are set by the caller after parsing.
pub fn parse_conventional_commit(hash: &str, message: &str) -> Option<ConventionalCommit> {
    let re = Regex::new(
        r"^(?P<type>[a-zA-Z]+)(?:\((?P<scope>[^)]+)\))?(?P<bang>!)?:\s*(?P<desc>.+)"
    ).unwrap();

    let first_line = message.lines().next()?;
    let caps = re.captures(first_line)?;

    let commit_type = caps.name("type")?.as_str().to_lowercase();
    let scope = caps.name("scope").map(|m| m.as_str().to_string());
    let description = caps.name("desc")?.as_str().trim().to_string();
    let bang = caps.name("bang").is_some();

    // Extract body (everything after first blank line)
    let body = message
        .splitn(2, "\n\n")
        .nth(1)
        .map(|b| b.trim().to_string())
        .filter(|b| !b.is_empty());

    // Check for BREAKING CHANGE in footer or bang notation
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
    fn test_body_extraction() {
        let msg = "feat: something\n\nThis is the body\nwith multiple lines";
        let c = parse_conventional_commit("abc123", msg).unwrap();
        assert_eq!(c.body.as_deref(), Some("This is the body\nwith multiple lines"));
    }
}
