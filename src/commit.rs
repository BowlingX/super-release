use regex::Regex;
use std::collections::HashSet;
use std::fmt;
use std::sync::LazyLock;

static CONVENTIONAL_COMMIT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?P<type>[a-zA-Z]+)(?:\((?P<scope>[^)]+)\))?(?P<bang>!)?:\s*(?P<desc>.+)")
        .unwrap()
});

/// Port of semantic-release's `revertPattern`: `Revert "<subject>"` or
/// `revert: <subject>`, followed by a `This reverts commit <hash>` footer.
static REVERT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)^(?:Revert|revert:)\s"?([\s\S]+?)"?\s*This reverts commit (\w{7,40})\b"#)
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

/// Info parsed from a revert commit's `This reverts commit <hash>` footer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevertInfo {
    /// Subject line of the reverted commit, as quoted in the revert message.
    pub header: String,
    /// Hash of the reverted commit as written in the footer (7-40 chars;
    /// `git revert` writes the full 40-char sha).
    pub hash: String,
}

fn parse_revert(message: &str) -> Option<RevertInfo> {
    let caps = REVERT_RE.captures(message)?;
    Some(RevertInfo {
        header: caps[1].trim().to_string(),
        hash: caps[2].to_string(),
    })
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
    /// Present when the message matches the semantic-release revert pattern.
    pub revert: Option<RevertInfo>,
    pub raw_message: String,
    /// Files changed by this commit (relative paths).
    pub files_changed: Vec<String>,
}

impl ConventionalCommit {
    /// The message with a git-style `Revert "..."` subject rewritten to
    /// `revert: <subject>` (git-cliff filters unconventional subjects).
    /// `raw_message` must stay untouched: [`filter_reverted_commits`]
    /// matches revert-of-revert on original subject lines.
    pub fn normalized_message(&self) -> String {
        let subject_is_conventional = self
            .raw_message
            .lines()
            .next()
            .is_some_and(|l| CONVENTIONAL_COMMIT_RE.is_match(l));
        match &self.revert {
            Some(info) if !subject_is_conventional => {
                let subject = info.header.lines().next().unwrap_or("");
                match self.raw_message.split_once('\n') {
                    Some((_, rest)) => format!("revert: {subject}\n{rest}"),
                    None => format!("revert: {subject}"),
                }
            }
            _ => self.raw_message.clone(),
        }
    }
}

/// Parse `<type>(<scope>)!: <description>`; `hash` and `files_changed` are set by the caller after parsing.
///
/// Git-style `Revert "<subject>"` messages have no conventional header but are
/// still accepted when they carry the `This reverts commit <hash>` footer
/// (semantic-release parity).
pub fn parse_conventional_commit(hash: &str, message: &str) -> Option<ConventionalCommit> {
    let first_line = message.lines().next()?;
    let revert = parse_revert(message);

    let (commit_type, scope, description, bang) = match CONVENTIONAL_COMMIT_RE.captures(first_line)
    {
        Some(caps) => (
            caps.name("type")?.as_str().to_lowercase(),
            caps.name("scope").map(|m| m.as_str().to_string()),
            caps.name("desc")?.as_str().trim().to_string(),
            caps.name("bang").is_some(),
        ),
        None => (
            "revert".to_string(),
            None,
            revert.as_ref()?.header.clone(),
            false,
        ),
    };

    let body = message
        .split_once("\n\n")
        .map(|(_, b)| b.trim().to_string())
        .filter(|b| !b.is_empty());

    let breaking =
        bang || message.contains("BREAKING CHANGE:") || message.contains("BREAKING-CHANGE:");

    let bump = if breaking {
        BumpLevel::Major
    } else if revert.is_some() {
        // semantic-release's revert rule keys on the footer, not the type.
        BumpLevel::Patch
    } else {
        match commit_type.as_str() {
            "feat" => BumpLevel::Minor,
            "fix" | "perf" => BumpLevel::Patch,
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
        revert,
        raw_message: message.to_string(),
        files_changed: Vec::new(),
    })
}

/// Drop each revert commit together with the commit it reverts when both are
/// in the analyzed range (semantic-release's reverted-commit filtering).
///
/// Input must be newest-first: a held revert cancels the first older commit
/// matching its captured header + full sha, and dies with its own revert info
/// when cancelled itself (revert-of-revert keeps the original). Unmatched
/// reverts are kept.
pub fn filter_reverted_commits(commits: Vec<ConventionalCommit>) -> Vec<ConventionalCommit> {
    let mut removed: HashSet<usize> = HashSet::new();
    let mut held: Vec<usize> = Vec::new();

    for (i, commit) in commits.iter().enumerate() {
        if !held.is_empty() {
            let header = commit.raw_message.lines().next().unwrap_or("").trim();
            let full_hash = commit.oid.map(|o| o.to_string());

            let matched = held.iter().position(|&ri| {
                let info = commits[ri]
                    .revert
                    .as_ref()
                    .expect("held commits carry revert info");
                info.header == header && full_hash.as_deref() == Some(info.hash.as_str())
            });
            if let Some(pos) = matched {
                removed.insert(held.remove(pos));
                removed.insert(i);
                continue;
            }
        }
        if commit.revert.is_some() {
            held.push(i);
        }
    }

    if removed.is_empty() {
        return commits;
    }
    commits
        .into_iter()
        .enumerate()
        .filter(|(i, _)| !removed.contains(i))
        .map(|(_, c)| c)
        .collect()
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
            // No `This reverts commit <sha>` footer → no release (semantic-release parity).
            "revert: undo thing",
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

    const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SHA_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const SHA_C: &str = "cccccccccccccccccccccccccccccccccccccccc";

    #[test]
    fn test_parse_git_style_revert() {
        let msg = format!("Revert \"feat: add login\"\n\nThis reverts commit {SHA_A}.");
        let c = parse_conventional_commit("abc123", &msg).unwrap();
        assert_eq!(c.commit_type, "revert");
        assert_eq!(c.description, "feat: add login");
        assert_eq!(c.bump, BumpLevel::Patch);
        assert!(!c.breaking);
        assert_eq!(
            c.revert,
            Some(RevertInfo {
                header: "feat: add login".into(),
                hash: SHA_A.into(),
            })
        );
    }

    #[test]
    fn test_parse_angular_style_revert() {
        let msg = format!("revert: feat: add login\n\nThis reverts commit {SHA_A}.");
        let c = parse_conventional_commit("abc123", &msg).unwrap();
        assert_eq!(c.commit_type, "revert");
        assert_eq!(c.bump, BumpLevel::Patch);
        assert_eq!(
            c.revert,
            Some(RevertInfo {
                header: "feat: add login".into(),
                hash: SHA_A.into(),
            })
        );
    }

    #[test]
    fn test_bare_revert_without_footer() {
        let c = parse_conventional_commit("abc123", "revert: undo thing").unwrap();
        assert!(c.revert.is_none());
        assert_eq!(c.bump, BumpLevel::None);
    }

    #[test]
    fn test_revert_regex_case_insensitive() {
        let msg = format!("REVERT \"feat: x\"\n\nthis reverts commit {SHA_A}.");
        let c = parse_conventional_commit("abc123", &msg).unwrap();
        assert!(c.revert.is_some());
        assert_eq!(c.bump, BumpLevel::Patch);
    }

    #[test]
    fn test_revert_of_revert_header_keeps_inner_quotes() {
        let msg = format!("Revert \"Revert \"feat: x\"\"\n\nThis reverts commit {SHA_B}.");
        let c = parse_conventional_commit("abc123", &msg).unwrap();
        assert_eq!(c.revert.unwrap().header, "Revert \"feat: x\"");
    }

    #[test]
    fn test_revert_short_hash_captured() {
        let msg = "Revert \"fix: y\"\n\nThis reverts commit abcdef0.";
        let c = parse_conventional_commit("abc123", msg).unwrap();
        assert_eq!(c.revert.unwrap().hash, "abcdef0");
    }

    #[test]
    fn test_revert_subject_without_footer_still_none() {
        assert!(parse_conventional_commit("abc123", "Revert \"feat: x\"").is_none());
    }

    #[test]
    fn test_normalized_message_rewrites_only_git_style_reverts() {
        let git_style = format!("Revert \"feat: add login\"\n\nThis reverts commit {SHA_A}.");
        let c = parse_conventional_commit("abc123", &git_style).unwrap();
        assert_eq!(
            c.normalized_message(),
            format!("revert: feat: add login\n\nThis reverts commit {SHA_A}.")
        );

        let angular = format!("revert: feat: add login\n\nThis reverts commit {SHA_A}.");
        let c = parse_conventional_commit("abc123", &angular).unwrap();
        assert_eq!(c.normalized_message(), angular);

        let plain = parse_conventional_commit("abc123", "feat: x").unwrap();
        assert_eq!(plain.normalized_message(), "feat: x");
    }

    /// Parse and attach the full oid, as production commits have (src/git.rs).
    fn oc(sha40: &str, msg: &str) -> ConventionalCommit {
        let mut c = parse_conventional_commit(&sha40[..8], msg).unwrap();
        c.oid = Some(git2::Oid::from_str(sha40).unwrap());
        c
    }

    fn revert_of(target_header: &str, target_sha: &str) -> String {
        format!("Revert \"{target_header}\"\n\nThis reverts commit {target_sha}.")
    }

    #[test]
    fn test_filter_cancels_revert_pair() {
        let commits = vec![
            oc(SHA_B, &revert_of("feat: add login", SHA_A)),
            oc(SHA_A, "feat: add login"),
        ];
        assert!(filter_reverted_commits(commits).is_empty());
    }

    #[test]
    fn test_filter_angular_revert_cancels_pair() {
        let msg = format!("revert: feat: add login\n\nThis reverts commit {SHA_A}.");
        let commits = vec![oc(SHA_B, &msg), oc(SHA_A, "feat: add login")];
        assert!(filter_reverted_commits(commits).is_empty());
    }

    #[test]
    fn test_filter_revert_of_revert_keeps_original() {
        let revert = revert_of("feat: add login", SHA_A);
        let revert_of_revert = revert_of(revert.lines().next().unwrap(), SHA_B);
        let commits = vec![
            oc(SHA_C, &revert_of_revert),
            oc(SHA_B, &revert),
            oc(SHA_A, "feat: add login"),
        ];
        let kept = filter_reverted_commits(commits);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].raw_message, "feat: add login");
    }

    #[test]
    fn test_filter_keeps_unmatched_revert() {
        let commits = vec![oc(SHA_B, &revert_of("feat: add login", SHA_A))];
        let kept = filter_reverted_commits(commits);
        assert_eq!(kept.len(), 1);
        assert!(kept[0].revert.is_some());
    }

    #[test]
    fn test_filter_hash_mismatch_no_cancellation() {
        let commits = vec![
            oc(SHA_B, &revert_of("feat: add login", SHA_C)),
            oc(SHA_A, "feat: add login"),
        ];
        assert_eq!(filter_reverted_commits(commits).len(), 2);
    }

    #[test]
    fn test_filter_header_mismatch_no_cancellation() {
        let commits = vec![
            oc(SHA_B, &revert_of("feat: add logout", SHA_A)),
            oc(SHA_A, "feat: add login"),
        ];
        assert_eq!(filter_reverted_commits(commits).len(), 2);
    }

    #[test]
    fn test_filter_passes_non_reverts_through() {
        let commits = vec![
            oc(SHA_A, "feat: a"),
            oc(SHA_B, "fix: b"),
            oc(SHA_C, "chore: c"),
        ];
        let kept = filter_reverted_commits(commits);
        assert_eq!(kept.len(), 3);
        assert_eq!(kept[0].raw_message, "feat: a");
        assert_eq!(kept[2].raw_message, "chore: c");
    }

    /// A revert only cancels commits after it in the newest-first list;
    /// one that predates its target is not a revert of it.
    #[test]
    fn test_filter_revert_older_than_target_no_cancellation() {
        let commits = vec![
            oc(SHA_A, "feat: add login"),
            oc(SHA_B, &revert_of("feat: add login", SHA_A)),
        ];
        assert_eq!(filter_reverted_commits(commits).len(), 2);
    }
}
