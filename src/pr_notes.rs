//! Release notes drafted in pull-request comments. A comment starting with
//! `@super-release release-notes` (optionally `release-notes@<package>` or
//! `release-notes <package>`) replaces the generated notes of the commits its PR
//! introduced; a release carrying several PRs (e.g. develop → main) combines them.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

use crate::commit::ConventionalCommit;
use crate::config;
use crate::forge::github::metadata::{PrComment, PullRequestInfo, ReleaseAttribution, merge_order};
use crate::version::PackageRelease;

/// The directive line; a package follows either `release-notes@` or whitespace.
static DIRECTIVE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^@super-release\s+release-notes(?:@(\S+)|\s+(\S+))?\s*$")
        .expect("valid directive regex")
});

/// Carried by our own comments (preview, released), which are never directives.
const OWN_COMMENT_MARKER: &str = "<!-- super-release:";

/// A parsed `@super-release release-notes` comment.
#[derive(Debug, Clone, PartialEq)]
pub struct Directive {
    /// Package name or glob; `None` applies to every package the PR releases.
    pub target: Option<String>,
    /// Markdown notes; empty resets the PR to its generated notes.
    pub body: String,
}

/// The directive must be the comment's first non-empty line, so quoting one in a
/// reply (`> @super-release …`) doesn't count.
pub fn parse_directive(body: &str) -> Option<Directive> {
    if body.contains(OWN_COMMENT_MARKER) {
        return None;
    }
    let body = body.replace("\r\n", "\n");
    let rest = body.trim_start();
    let (first, remainder) = rest.split_once('\n').unwrap_or((rest, ""));
    let caps = DIRECTIVE.captures(first.trim())?;
    let target = caps
        .get(1)
        .or_else(|| caps.get(2))
        .map(|m| m.as_str().to_string());
    let notes: Vec<&str> = remainder
        .lines()
        .skip_while(|l| l.trim().is_empty())
        .collect();
    Some(Directive {
        target,
        body: notes.join("\n").trim_end().to_string(),
    })
}

/// What the PR-comment lookup found for a run.
#[derive(Debug, Default)]
pub struct ReleaseMetadata {
    pub attribution: ReleaseAttribution,
    pub pull_requests: Vec<PullRequestInfo>,
    /// Author associations whose directives count (`release_notes.trusted_authors`).
    pub trusted_authors: Vec<String>,
    /// Each released package's notes.
    notes: HashMap<String, PackageNotes>,
}

impl ReleaseMetadata {
    pub fn new(
        attribution: ReleaseAttribution,
        pull_requests: Vec<PullRequestInfo>,
        trusted_authors: Vec<String>,
        releases: &[PackageRelease],
    ) -> Self {
        let mut meta = Self {
            attribution,
            pull_requests,
            trusted_authors,
            notes: HashMap::new(),
        };
        meta.notes = releases
            .iter()
            .map(|r| (r.package_name.clone(), package_notes(r, &meta)))
            .collect();
        meta
    }

    /// The PR-authored notes of a released package.
    pub fn notes_for(&self, package: &str) -> Option<&PackageNotes> {
        self.notes.get(package)
    }
}

/// One PR's notes as rendered into a release (`extra.release_notes` in templates).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NoteBlock {
    pub pr_number: i64,
    pub pr_title: String,
    pub pr_url: String,
    /// Who wrote the notes (the directive comment's author).
    pub author: Option<String>,
    pub comment_url: String,
    pub body: String,
}

/// A package release's PR-authored notes and the commits they replace.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PackageNotes {
    /// Release PRs (carrying other PRs' commits) first, then by merge order.
    pub blocks: Vec<NoteBlock>,
    /// Full SHAs of the commits whose generated lines the notes replace.
    pub replaced: HashSet<String>,
}

impl PackageNotes {
    pub fn replaces(&self, commit: &ConventionalCommit) -> bool {
        commit
            .full_sha()
            .is_some_and(|sha| self.replaced.contains(&sha))
    }
}

fn is_trusted(comment: &PrComment, trusted: &[String]) -> bool {
    trusted
        .iter()
        .any(|t| t.eq_ignore_ascii_case(&comment.association))
}

fn targets(target: &str, package: &str) -> bool {
    target == package || config::glob_match(target, package)
}

/// The newest trusted directive for `package`; a targeted one beats an untargeted one.
fn active_directive<'a>(
    pr: &'a PullRequestInfo,
    package: &str,
    trusted: &[String],
) -> Option<(&'a PrComment, Directive)> {
    let mut comments: Vec<&PrComment> = pr
        .comments
        .iter()
        .filter(|c| is_trusted(c, trusted))
        .collect();
    comments.sort_by(|a, b| a.written_at.cmp(&b.written_at));
    let mut targeted = None;
    let mut untargeted = None;
    for comment in comments {
        let Some(directive) = parse_directive(&comment.body) else {
            continue;
        };
        match &directive.target {
            Some(target) if targets(target, package) => targeted = Some((comment, directive)),
            Some(_) => {}
            None => untargeted = Some((comment, directive)),
        }
    }
    targeted.or(untargeted)
}

/// Each commit belongs to its earliest-merged PR, whose notes replace its line.
/// Release PRs (carrying other PRs' commits) come first as the intro.
fn package_notes(release: &PackageRelease, meta: &ReleaseMetadata) -> PackageNotes {
    let prs: HashMap<i64, &PullRequestInfo> =
        meta.pull_requests.iter().map(|p| (p.number, p)).collect();
    let by_merge = |a: &i64, b: &i64| {
        merge_order(prs[a].merged_at.as_deref(), prs[b].merged_at.as_deref()).then(a.cmp(b))
    };

    let mut linked: Vec<i64> = Vec::new();
    let mut carriers: HashSet<i64> = HashSet::new();
    let mut owners: Vec<(String, i64)> = Vec::new();
    for commit in &release.commits {
        let Some(sha) = commit.full_sha() else {
            continue;
        };
        let mut containing: Vec<i64> = meta
            .attribution
            .commits
            .get(&sha)
            .into_iter()
            .flat_map(|a| a.pull_requests.iter().map(|pr| pr.number))
            .chain(
                meta.pull_requests
                    .iter()
                    .filter(|pr| pr.commits.contains(&sha))
                    .map(|pr| pr.number),
            )
            .filter(|n| prs.contains_key(n))
            .collect();
        containing.sort_by(by_merge);
        containing.dedup();
        if let Some((&owner, rest)) = containing.split_first() {
            owners.push((sha, owner));
            carriers.extend(rest);
        }
        linked.extend(containing);
    }
    linked.sort_by(by_merge);
    linked.dedup();

    let notes: HashMap<i64, NoteBlock> = linked
        .iter()
        .filter_map(|n| {
            let pr = prs[n];
            let (comment, directive) =
                active_directive(pr, &release.package_name, &meta.trusted_authors)?;
            (!directive.body.is_empty()).then(|| {
                (
                    *n,
                    NoteBlock {
                        pr_number: pr.number,
                        pr_title: pr.title.clone(),
                        pr_url: pr.url.clone(),
                        author: comment.author.clone(),
                        comment_url: comment.url.clone(),
                        body: directive.body,
                    },
                )
            })
        })
        .collect();

    // Stable: merge order holds within the intro and the rest.
    linked.sort_by_key(|n| !carriers.contains(n));
    PackageNotes {
        blocks: linked
            .iter()
            .filter_map(|n| notes.get(n).cloned())
            .collect(),
        replaced: owners
            .into_iter()
            .filter(|(_, owner)| notes.contains_key(owner))
            .map(|(sha, _)| sha)
            .collect(),
    }
}

/// Why a directive comment doesn't count.
#[derive(Debug, Clone, PartialEq)]
pub enum IgnoreReason {
    /// The author's association isn't in `release_notes.trusted_authors`.
    Untrusted,
    /// The target matches no released package that gets release notes.
    UnknownPackage(String),
}

/// A directive comment found on one of the run's PRs.
#[derive(Debug, Clone, PartialEq)]
pub struct DirectiveComment<'a> {
    pub pr: i64,
    pub comment: &'a PrComment,
    pub ignored: Option<IgnoreReason>,
}

/// Every directive comment on the run's PRs, and why it is ignored, if it is.
pub fn directive_comments(meta: &ReleaseMetadata) -> Vec<DirectiveComment<'_>> {
    meta.pull_requests
        .iter()
        .flat_map(|pr| pr.comments.iter().map(move |c| (pr.number, c)))
        .filter_map(|(pr, comment)| {
            let directive = parse_directive(&comment.body)?;
            let ignored = if is_trusted(comment, &meta.trusted_authors) {
                directive
                    .target
                    .filter(|t| !meta.notes.keys().any(|p| targets(t, p)))
                    .map(IgnoreReason::UnknownPackage)
            } else {
                Some(IgnoreReason::Untrusted)
            };
            Some(DirectiveComment {
                pr,
                comment,
                ignored,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commit::BumpLevel;
    use crate::forge::github::metadata::{CommitAttribution, PullRequestRef};

    fn directive(target: Option<&str>, body: &str) -> Option<Directive> {
        Some(Directive {
            target: target.map(String::from),
            body: body.into(),
        })
    }

    #[test]
    fn parses_untargeted_directive_with_markdown_body() {
        assert_eq!(
            parse_directive("@super-release release-notes\r\n- This\r\n- Is some **markdown**\r\n"),
            directive(None, "- This\n- Is some **markdown**")
        );
    }

    #[test]
    fn parses_package_targets() {
        assert_eq!(
            parse_directive("@super-release release-notes@core\nx"),
            directive(Some("core"), "x")
        );
        assert_eq!(
            parse_directive("@super-release release-notes @acme/ui\nx"),
            directive(Some("@acme/ui"), "x")
        );
        assert_eq!(
            parse_directive("@super-release release-notes@@acme/ui\nx"),
            directive(Some("@acme/ui"), "x")
        );
        assert_eq!(
            parse_directive("@Super-Release Release-Notes  @acme/*  \nx"),
            directive(Some("@acme/*"), "x")
        );
    }

    #[test]
    fn keeps_body_indentation_and_drops_surrounding_blank_lines() {
        assert_eq!(
            parse_directive("\n\n  @super-release release-notes\n\n\n    code\n- item\n\n"),
            directive(None, "    code\n- item")
        );
    }

    #[test]
    fn empty_body_is_a_reset() {
        assert_eq!(
            parse_directive("@super-release release-notes"),
            directive(None, "")
        );
    }

    #[test]
    fn ignores_non_directives() {
        for body in [
            "LGTM",
            "> @super-release release-notes\nquoted",
            "Thanks!\n@super-release release-notes\nnot first line",
            "@super-release release-notes-ish",
            "@super-release release-notes a b",
            "<!-- super-release:preview -->\n@super-release release-notes",
        ] {
            assert_eq!(parse_directive(body), None, "{body:?}");
        }
    }

    // --- resolution ---

    fn sha(c: char) -> String {
        std::iter::repeat_n(c, 40).collect()
    }

    fn commit(c: char, message: &str) -> ConventionalCommit {
        crate::test_fixtures::oc(&sha(c), message)
    }

    fn release(name: &str, commits: Vec<crate::commit::ConventionalCommit>) -> PackageRelease {
        PackageRelease {
            package_name: name.into(),
            current_version: semver::Version::new(1, 3, 0),
            next_version: semver::Version::new(1, 4, 0),
            bump: BumpLevel::Minor,
            commits,
            is_root: true,
            propagated_from: None,
        }
    }

    /// A comment written at second `id`, so ids give chronological order.
    fn note(id: u64, association: &str, body: &str) -> PrComment {
        PrComment {
            id,
            author: Some(format!("user{id}")),
            association: association.into(),
            body: body.into(),
            url: format!("https://github.com/o/r/pull/x#issuecomment-{id}"),
            written_at: format!("2026-03-01T00:00:{id:02}Z"),
        }
    }

    fn pr(
        number: i64,
        merged_at: Option<&str>,
        commits: &[char],
        comments: Vec<PrComment>,
    ) -> PullRequestInfo {
        PullRequestInfo {
            number,
            title: format!("PR {number}"),
            url: format!("https://github.com/o/r/pull/{number}"),
            merged_at: merged_at.map(String::from),
            commits: commits.iter().map(|c| sha(*c)).collect(),
            comments,
        }
    }

    /// Attribution as GitHub reports it: each commit's merged PRs.
    fn attribution(links: &[(char, &[(i64, &str)])]) -> ReleaseAttribution {
        ReleaseAttribution {
            commits: links
                .iter()
                .map(|(c, prs)| {
                    (
                        sha(*c),
                        CommitAttribution {
                            login: None,
                            pull_requests: prs
                                .iter()
                                .map(|(number, merged_at)| PullRequestRef {
                                    number: *number,
                                    title: String::new(),
                                    labels: vec![],
                                    merged_at: Some(merged_at.to_string()),
                                })
                                .collect(),
                        },
                    )
                })
                .collect(),
            first_time: Default::default(),
        }
    }

    fn meta(
        attribution: ReleaseAttribution,
        pull_requests: Vec<PullRequestInfo>,
    ) -> ReleaseMetadata {
        ReleaseMetadata {
            attribution,
            pull_requests,
            trusted_authors: config::ReleaseNotesConfig::default().trusted_authors,
            notes: HashMap::new(),
        }
    }

    /// `meta` with notes resolved for `releases`, as the run builds it.
    fn resolved(meta: ReleaseMetadata, releases: &[PackageRelease]) -> ReleaseMetadata {
        ReleaseMetadata::new(
            meta.attribution,
            meta.pull_requests,
            meta.trusted_authors,
            releases,
        )
    }

    fn numbers(notes: &PackageNotes) -> Vec<i64> {
        notes.blocks.iter().map(|b| b.pr_number).collect()
    }

    const D12: &str = "2026-01-12T00:00:00Z";
    const D14: &str = "2026-01-14T00:00:00Z";
    const D15: &str = "2026-01-15T00:00:00Z";
    const D20: &str = "2026-02-01T00:00:00Z";

    /// develop → main: feature PRs #12 (no notes), #14, #15 and a direct commit `d`
    /// on develop, released through #20, which carries all of them.
    fn develop_to_main() -> (PackageRelease, ReleaseMetadata) {
        let release = release(
            "root",
            vec![
                commit('a', "feat: dark mode"),
                commit('b', "feat: plugin api"),
                commit('c', "fix: startup"),
                commit('d', "chore: tidy config"),
            ],
        );
        let meta = meta(
            attribution(&[
                ('a', &[(12, D12), (20, D20)]),
                ('b', &[(14, D14), (20, D20)]),
                ('c', &[(15, D15)]),
                ('d', &[(20, D20)]),
            ]),
            vec![
                pr(12, Some(D12), &['a'], vec![]),
                pr(
                    14,
                    Some(D14),
                    &['b'],
                    vec![
                        note(1, "MEMBER", "@super-release release-notes\nfirst draft"),
                        note(2, "MEMBER", "@super-release release-notes\n**Plugin API**"),
                    ],
                ),
                pr(
                    15,
                    Some(D15),
                    &['c'],
                    vec![note(
                        3,
                        "OWNER",
                        "@super-release release-notes\nFaster startup",
                    )],
                ),
                // GitHub didn't link `c` to #20, but #20's commit list contains it.
                pr(
                    20,
                    Some(D20),
                    &['a', 'b', 'c', 'd'],
                    vec![note(
                        4,
                        "COLLABORATOR",
                        "@super-release release-notes\nThis release…",
                    )],
                ),
            ],
        );
        (release, meta)
    }

    #[test]
    fn combines_prs_with_the_release_pr_as_intro_and_replaces_owned_commits() {
        let (release, meta) = develop_to_main();
        let notes = package_notes(&release, &meta);

        assert_eq!(numbers(&notes), vec![20, 14, 15]);
        assert_eq!(
            notes.blocks[1].body, "**Plugin API**",
            "newest comment wins"
        );
        assert_eq!(notes.blocks[1].author.as_deref(), Some("user2"));
        // #12 has no notes, so only its commit keeps its generated line.
        assert_eq!(
            notes.replaced,
            [sha('b'), sha('c'), sha('d')].into_iter().collect()
        );
    }

    #[test]
    fn targeted_directive_beats_untargeted_and_only_for_its_package() {
        let release_ui = release("@acme/ui", vec![commit('a', "feat: x")]);
        let release_core = release("@acme/core", vec![commit('a', "feat: x")]);
        let meta = meta(
            attribution(&[('a', &[(7, D14)])]),
            vec![pr(
                7,
                Some(D14),
                &['a'],
                vec![
                    note(
                        1,
                        "MEMBER",
                        "@super-release release-notes@@acme/ui\nui notes",
                    ),
                    note(2, "MEMBER", "@super-release release-notes\ngeneral notes"),
                ],
            )],
        );

        assert_eq!(package_notes(&release_ui, &meta).blocks[0].body, "ui notes");
        assert_eq!(
            package_notes(&release_core, &meta).blocks[0].body,
            "general notes"
        );
    }

    #[test]
    fn untrusted_directives_are_ignored() {
        let release = release("root", vec![commit('a', "feat: x")]);
        let meta = meta(
            attribution(&[('a', &[(7, D14)])]),
            vec![pr(
                7,
                Some(D14),
                &['a'],
                vec![
                    note(1, "MEMBER", "@super-release release-notes\nreal"),
                    note(2, "CONTRIBUTOR", "@super-release release-notes\nspam"),
                ],
            )],
        );

        let notes = package_notes(&release, &meta);
        assert_eq!(notes.blocks[0].body, "real");

        let meta = resolved(meta, std::slice::from_ref(&release));
        let found = directive_comments(&meta);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].ignored, None);
        assert_eq!(found[1].ignored, Some(IgnoreReason::Untrusted));
    }

    #[test]
    fn newest_empty_directive_resets_to_generated_notes() {
        let release = release("root", vec![commit('a', "feat: x")]);
        let meta = meta(
            attribution(&[('a', &[(7, D14)])]),
            vec![pr(
                7,
                Some(D14),
                &['a'],
                vec![
                    note(1, "MEMBER", "@super-release release-notes\nnotes"),
                    note(2, "MEMBER", "@super-release release-notes"),
                ],
            )],
        );

        assert_eq!(package_notes(&release, &meta), PackageNotes::default());
    }

    /// Editing an older directive makes it the newest one again.
    #[test]
    fn edited_directive_wins_over_later_comments() {
        let release = release("root", vec![commit('a', "feat: x")]);
        let mut edited = note(1, "MEMBER", "@super-release release-notes\nedited");
        edited.written_at = "2026-03-02T00:00:00Z".into();
        let meta = meta(
            attribution(&[('a', &[(7, D14)])]),
            vec![pr(
                7,
                Some(D14),
                &['a'],
                vec![
                    edited,
                    note(2, "MEMBER", "@super-release release-notes\nlater"),
                ],
            )],
        );

        assert_eq!(package_notes(&release, &meta).blocks[0].body, "edited");
    }

    /// An edit that removes the directive line drops the comment; the previous
    /// directive applies again.
    #[test]
    fn directive_edited_into_plain_comment_falls_back() {
        let release = release("root", vec![commit('a', "feat: x")]);
        let meta = meta(
            attribution(&[('a', &[(7, D14)])]),
            vec![pr(
                7,
                Some(D14),
                &['a'],
                vec![
                    note(1, "MEMBER", "@super-release release-notes\nolder"),
                    note(2, "MEMBER", "never mind"),
                ],
            )],
        );

        assert_eq!(package_notes(&release, &meta).blocks[0].body, "older");
    }

    /// In a preview the PR is still open: it owns its commits through its commit
    /// list, after any merged PR.
    #[test]
    fn open_pr_owns_its_commits_in_a_preview() {
        let release = release(
            "root",
            vec![
                commit('a', "feat: merged earlier"),
                commit('b', "feat: in this pr"),
            ],
        );
        let meta = meta(
            attribution(&[('a', &[(5, D12)])]),
            vec![
                pr(5, Some(D12), &['a'], vec![]),
                pr(
                    9,
                    None,
                    &['b'],
                    vec![note(1, "OWNER", "@super-release release-notes\nmine")],
                ),
            ],
        );

        let notes = package_notes(&release, &meta);
        assert_eq!(numbers(&notes), vec![9]);
        assert_eq!(notes.replaced, [sha('b')].into_iter().collect());
    }

    #[test]
    fn unknown_package_targets_are_reported() {
        let meta = meta(
            ReleaseAttribution::default(),
            vec![pr(
                7,
                None,
                &[],
                vec![
                    note(1, "MEMBER", "@super-release release-notes@typo\nx"),
                    note(2, "MEMBER", "@super-release release-notes@@acme/*\nx"),
                ],
            )],
        );

        let meta = resolved(meta, &[release("@acme/ui", vec![])]);
        let found = directive_comments(&meta);
        assert_eq!(
            found[0].ignored,
            Some(IgnoreReason::UnknownPackage("typo".into()))
        );
        assert_eq!(found[1].ignored, None);
    }

    #[test]
    fn no_pull_requests_means_no_notes() {
        let (release, mut meta) = develop_to_main();
        meta.pull_requests.clear();
        assert_eq!(package_notes(&release, &meta), PackageNotes::default());
    }

    /// Resolved once up front; looked up by package name.
    #[test]
    fn metadata_resolves_each_release_once() {
        let (release, meta) = develop_to_main();
        let resolved = ReleaseMetadata::new(
            meta.attribution,
            meta.pull_requests,
            meta.trusted_authors,
            std::slice::from_ref(&release),
        );

        let notes = resolved.notes_for("root").unwrap();
        assert_eq!(numbers(notes), vec![20, 14, 15]);
        assert!(notes.replaces(&release.commits[1]));
        assert!(!notes.replaces(&release.commits[0]));
        assert_eq!(resolved.notes_for("other"), None);
    }
}
