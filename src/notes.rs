//! Release-notes engine: one git-cliff-backed source of markdown notes shared by
//! the changelog, github, and PR-preview steps.

use anyhow::Result;
use std::sync::LazyLock;

use git_cliff_core::changelog::Changelog;
use git_cliff_core::commit::Commit as CliffCommit;
use git_cliff_core::config::{Config as CliffConfig, Remote};
use git_cliff_core::contributor::RemoteContributor;
use git_cliff_core::release::Release as CliffRelease;

use crate::commit::ConventionalCommit;
use crate::forge::github::metadata::ReleaseAttribution;
use crate::pr_notes::PackageNotes;
use crate::version::PackageRelease;

/// Added to the built-in bodies: a release forced by a dependency bump has no
/// commits of its own, so this says which dependency caused it.
const DEPENDENCY_UPDATE_NOTE: &str = include_str!("../templates/dependency-update.tera");
const DEPENDENCY_UPDATE_MARKER: &str = "{# dependency-update #}";

/// PR-comment release notes (`extra.release_notes`), above the commit groups.
const RELEASE_NOTES_SECTION: &str = include_str!("../templates/release-notes.tera");
const RELEASE_NOTES_MARKER: &str = "{# release-notes #}";

/// git-cliff's default body, vendored to hold the markers.
const CHANGELOG_BODY: &str = include_str!("../templates/changelog-body.tera");

/// Compare/PR links use `extra.repo_url`/`extra.tag`/`extra.previous_tag` so they
/// point at real tag names, not the bare version. Contributor credits are derived
/// from the rendered commits, not `github.contributors`, which also counts commits
/// skipped by the commit parsers (e.g. `chore(deps)` bumps).
const GITHUB_GROUPED_BODY: &str = include_str!("../templates/github-release-body.tera");

/// git-cliff's default config with one of our bodies, its markers filled in.
fn cliff_config(body: &str) -> CliffConfig {
    let mut config: CliffConfig = "".parse().expect("Failed to load git-cliff default config");
    config.changelog.body = body
        .replace(DEPENDENCY_UPDATE_MARKER, DEPENDENCY_UPDATE_NOTE.trim_end())
        .replace(RELEASE_NOTES_MARKER, RELEASE_NOTES_SECTION.trim_end());
    config
}

static CLIFF_CONFIG: LazyLock<CliffConfig> = LazyLock::new(|| cliff_config(CHANGELOG_BODY));

/// Parsed once; the GitHub remote is set on the clone per release.
static GITHUB_CLIFF_CONFIG: LazyLock<CliffConfig> =
    LazyLock::new(|| cliff_config(GITHUB_GROUPED_BODY));

/// Uses the full commit SHA as the id because git-cliff matches commits to PRs by SHA.
pub fn to_cliff_commits(commits: &[ConventionalCommit]) -> Vec<CliffCommit<'_>> {
    commits
        .iter()
        .map(|c| {
            let id = c.full_sha().unwrap_or_else(|| c.hash.clone());
            // git-cliff's default config filters unconventional subjects, which
            // would silently drop git-style reverts from rendered notes.
            CliffCommit::new(id, c.normalized_message())
        })
        .collect()
}

/// `template` overrides the default (grouped conventional) body.
pub fn generate_release_notes(
    release: &PackageRelease,
    template: Option<&str>,
    notes: Option<&PackageNotes>,
) -> Result<String> {
    let mut config = CLIFF_CONFIG.clone();
    if let Some(body) = template {
        config.changelog.body = body.to_string();
    }
    render_changelog(config, plain_release(release, notes))
}

/// Flags commits whose line PR notes replace with `extra.replaced`; the built-in
/// bodies skip them.
fn release_commits<'a>(
    release: &'a PackageRelease,
    notes: Option<&PackageNotes>,
) -> Vec<CliffCommit<'a>> {
    let mut commits = to_cliff_commits(&release.commits);
    for (cliff, commit) in commits.iter_mut().zip(&release.commits) {
        let replaced = notes.is_some_and(|n| n.replaces(commit));
        cliff.extra = Some(serde_json::json!({ "replaced": replaced }));
    }
    commits
}

/// Build a git-cliff release from ours, for offline (no-remote) rendering.
fn plain_release<'a>(
    release: &'a PackageRelease,
    notes: Option<&PackageNotes>,
) -> CliffRelease<'a> {
    CliffRelease {
        version: Some(release.next_version.to_string()),
        commits: release_commits(release, notes),
        timestamp: Some(chrono::Local::now().timestamp()),
        previous: Some(Box::new(CliffRelease {
            version: Some(release.current_version.to_string()),
            ..Default::default()
        })),
        extra: Some(serde_json::json!({
            "dependency_chain": &release.propagated_from,
            "release_notes": notes.map(|n| &n.blocks),
        })),
        ..Default::default()
    }
}

/// The GitHub repository a release belongs to, for the template context and links.
pub struct GithubContext<'a> {
    pub owner: &'a str,
    pub repo: &'a str,
    /// GitHub Enterprise API base URL, if any.
    pub api_url: Option<&'a str>,
    /// The release's HEAD commit SHA.
    pub head_commit_id: Option<String>,
    /// The repo's web URL (`https://host/owner/repo`) for PR and compare links.
    pub web_url: &'a str,
}

impl GithubContext<'_> {
    fn remote(&self) -> Remote {
        Remote {
            owner: self.owner.to_string(),
            repo: self.repo.to_string(),
            api_url: self.api_url.map(String::from),
            ..Default::default()
        }
    }
}

/// `tag`/`previous_tag` are the real tag names, needed for a correct compare link
/// with prefixed tags. Without `attribution` the notes keep their grouping and
/// compare link but credit no contributors or PRs.
pub fn generate_release_notes_with_github(
    release: &PackageRelease,
    gh: &GithubContext,
    attribution: Option<&ReleaseAttribution>,
    tag: &str,
    previous_tag: &str,
    template: Option<&str>,
    notes: Option<&PackageNotes>,
) -> Result<String> {
    let mut config = GITHUB_CLIFF_CONFIG.clone();
    if let Some(body) = template {
        config.changelog.body = body.to_string();
    }
    // Only feeds the `remote.github` template context; rendering stays offline.
    config.remote.github = gh.remote();

    let mut cliff_release = enriched_release(
        release,
        gh.head_commit_id.clone(),
        gh.web_url,
        tag,
        previous_tag,
        notes,
    );
    if let Some(attribution) = attribution {
        attribute(&mut cliff_release, attribution);
    }
    render_changelog(config, cliff_release)
}

/// Copies the run's GitHub attribution into the fields templates read:
/// `commit.remote` (and its deprecated `commit.github` alias) and `github.contributors`.
fn attribute(release: &mut CliffRelease, attribution: &ReleaseAttribution) {
    for commit in &mut release.commits {
        let Some(found) = attribution.commits.get(&commit.id) else {
            continue;
        };
        let pr = found.pull_request();
        let contributor = RemoteContributor {
            username: found.login.clone(),
            pr_title: pr.map(|pr| pr.title.clone()),
            pr_number: pr.map(|pr| pr.number),
            pr_labels: pr.map(|pr| pr.labels.clone()).unwrap_or_default(),
            is_first_time: found
                .login
                .as_ref()
                .is_some_and(|login| attribution.first_time.contains(login)),
        };
        if !release
            .github
            .contributors
            .iter()
            .any(|c| c.username == contributor.username)
        {
            release.github.contributors.push(contributor.clone());
        }
        #[allow(deprecated)]
        {
            commit.github = contributor.clone();
        }
        commit.remote = Some(contributor);
    }
}

/// Build a git-cliff release from ours, attaching the tag/URL data the template
/// needs via `extra`.
fn enriched_release<'a>(
    release: &'a PackageRelease,
    head_commit_id: Option<String>,
    web_url: &str,
    tag: &str,
    previous_tag: &str,
    notes: Option<&PackageNotes>,
) -> CliffRelease<'a> {
    CliffRelease {
        commit_id: head_commit_id,
        extra: Some(serde_json::json!({
            "repo_url": web_url,
            "tag": tag,
            "previous_tag": previous_tag,
            "dependency_chain": &release.propagated_from,
            "release_notes": notes.map(|n| &n.blocks),
        })),
        ..plain_release(release, notes)
    }
}

fn render_changelog(mut config: CliffConfig, release: CliffRelease) -> Result<String> {
    // git-cliff `.expect()`s its own remote fetch, and `GIT_CLIFF__*` env vars can enable one;
    // attribution comes from `GitHubForge::release_attribution` instead.
    config.remote.offline = true;
    let changelog = Changelog::new(vec![release], config, None)
        .map_err(|e| anyhow::anyhow!("Failed to create changelog: {}", e))?;

    let mut output = Vec::new();
    changelog
        .generate(&mut output)
        .map_err(|e| anyhow::anyhow!("Failed to generate changelog: {}", e))?;

    Ok(String::from_utf8(output)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commit::BumpLevel;
    use crate::test_fixtures::oc;

    /// GitHub commit↔PR matching is by full SHA, so the commit id must be the full OID, not the short hash.
    #[test]
    fn cliff_commits_use_full_sha() {
        let sha = "1234567890abcdef1234567890abcdef12345678";
        let commit = ConventionalCommit {
            hash: "12345678".into(),
            oid: Some(git2::Oid::from_str(sha).unwrap()),
            commit_type: "feat".into(),
            scope: None,
            description: "x".into(),
            body: None,
            breaking: false,
            bump: BumpLevel::Minor,
            revert: None,
            raw_message: "feat: x".into(),
            files_changed: vec![],
        };
        let commits = [commit];
        let cliff = to_cliff_commits(&commits);
        assert_eq!(cliff[0].id, sha);
    }

    const SHA: &str = "1234567890abcdef1234567890abcdef12345678";

    fn release_with(
        commits: Vec<ConventionalCommit>,
        propagated_from: Option<&[&str]>,
    ) -> PackageRelease {
        PackageRelease {
            package_name: "p".into(),
            current_version: semver::Version::new(1, 0, 0),
            next_version: semver::Version::new(1, 1, 0),
            bump: BumpLevel::Minor,
            commits,
            is_root: true,
            propagated_from: propagated_from
                .map(|chain| chain.iter().map(|s| s.to_string()).collect()),
        }
    }

    fn feat_commit() -> ConventionalCommit {
        oc(SHA, "feat: add a thing")
    }

    fn github_notes(release: &PackageRelease, attribution: Option<&ReleaseAttribution>) -> String {
        github_notes_with(release, attribution, None)
    }

    fn github_notes_with(
        release: &PackageRelease,
        attribution: Option<&ReleaseAttribution>,
        notes: Option<&PackageNotes>,
    ) -> String {
        let gh = GithubContext {
            owner: "o",
            repo: "r",
            api_url: None,
            head_commit_id: None,
            web_url: "https://github.com/o/r",
        };
        generate_release_notes_with_github(
            release,
            &gh,
            attribution,
            "p/v1.1.0",
            "p/v1.0.0",
            None,
            notes,
        )
        .unwrap()
    }

    fn pr_notes(bodies: &[&str], replaced: &[&str]) -> PackageNotes {
        PackageNotes {
            blocks: bodies
                .iter()
                .enumerate()
                .map(|(i, body)| crate::pr_notes::NoteBlock {
                    pr_number: i as i64 + 14,
                    pr_title: String::new(),
                    pr_url: String::new(),
                    author: Some("bob".into()),
                    comment_url: String::new(),
                    body: body.to_string(),
                })
                .collect(),
            replaced: replaced.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// PR notes render above the commit groups and replace their PR's commit lines.
    #[test]
    fn pr_notes_replace_their_commits_in_both_bodies() {
        let kept = oc("abcdef1234567890abcdef1234567890abcdef12", "fix: keep me");
        let release = release_with(vec![feat_commit(), kept], None);
        let notes = pr_notes(
            &["**Plugin API**: you can now …", "- Faster\n- Smaller"],
            &[SHA],
        );
        let github = github_notes_with(&release, Some(&octocat_attribution(false)), Some(&notes));
        // The replaced commit's author is still credited.
        assert!(
            github.contains("### 👥 Contributors\n\n- @octocat"),
            "{github}"
        );

        for rendered in [
            generate_release_notes(&release, None, Some(&notes)).unwrap(),
            github,
        ] {
            let section = rendered
                .find("### ✍️ Release notes")
                .unwrap_or_else(|| panic!("{rendered}"));
            let first = rendered.find("**Plugin API**").unwrap();
            let second = rendered.find("- Faster\n- Smaller").unwrap();
            let kept_at = rendered
                .find("Keep me")
                .unwrap_or_else(|| panic!("{rendered}"));
            assert!(rendered.find("## [1.1.0]").unwrap() < section, "{rendered}");
            assert!(
                section < first && first < second && second < kept_at,
                "{rendered}"
            );
            assert!(
                !rendered.contains("Add a thing"),
                "replaced commit rendered:\n{rendered}"
            );
        }
    }

    /// Custom templates get every commit, flagged with `extra.replaced`.
    #[test]
    fn custom_templates_see_which_commits_pr_notes_replace() {
        let kept = oc("abcdef1234567890abcdef1234567890abcdef12", "fix: keep me");
        let release = release_with(vec![feat_commit(), kept], None);
        let template = "{% for c in commits %}{{ c.message }}={{ c.extra.replaced }};{% endfor %}";
        let rendered =
            generate_release_notes(&release, Some(template), Some(&pr_notes(&["x"], &[SHA])))
                .unwrap();
        assert!(rendered.contains("add a thing=true;"), "{rendered}");
        assert!(rendered.contains("keep me=false;"), "{rendered}");
    }

    /// No notes, no section: the generated notes are unchanged.
    #[test]
    fn empty_pr_notes_render_nothing() {
        let release = release_with(vec![feat_commit()], None);
        let none = generate_release_notes(&release, None, None).unwrap();
        let empty = generate_release_notes(&release, None, Some(&PackageNotes::default())).unwrap();
        assert_eq!(none, empty);
        assert!(!none.contains("Release notes"), "{none}");
        assert_eq!(
            github_notes(&release, None),
            github_notes_with(&release, None, Some(&PackageNotes::default()))
        );
    }

    fn octocat_attribution(first_time: bool) -> ReleaseAttribution {
        use crate::forge::github::metadata::{CommitAttribution, PullRequestRef};
        ReleaseAttribution {
            commits: [(
                SHA.to_string(),
                CommitAttribution {
                    login: Some("octocat".into()),
                    pull_requests: vec![PullRequestRef {
                        number: 7,
                        title: "Add a thing".into(),
                        labels: vec![],
                        merged_at: None,
                    }],
                },
            )]
            .into(),
            first_time: if first_time {
                ["octocat".to_string()].into()
            } else {
                Default::default()
            },
        }
    }

    /// Attribution is keyed by the commit's own SHA, so a commit is credited even
    /// when its PR was merged under a different commit (rebase merges).
    #[test]
    fn attribution_credits_author_and_pr_of_each_commit() {
        let notes = github_notes(
            &release_with(vec![feat_commit()], None),
            Some(&octocat_attribution(false)),
        );
        assert!(
            notes.contains("by @octocat in [#7](https://github.com/o/r/pull/7)"),
            "author/PR attribution missing:\n{notes}"
        );
    }

    /// git-cliff only sees the release's own commits, which would make every author new.
    #[test]
    fn new_contributors_come_from_the_attribution() {
        let release = release_with(vec![feat_commit()], None);

        let returning = github_notes(&release, Some(&octocat_attribution(false)));
        assert!(!returning.contains("New Contributors"), "{returning}");

        let first = github_notes(&release, Some(&octocat_attribution(true)));
        assert!(
            first.contains("@octocat made their first contribution in [#7]"),
            "{first}"
        );
    }

    /// A dependency-only release names the dependency that forced it, above the compare link.
    #[test]
    fn dependency_only_release_says_which_dependency_changed() {
        let release = release_with(vec![], Some(&["a", "b"]));
        let note = "Released because its dependency `b` was updated (via `a` → `b`).";

        let changelog = generate_release_notes(&release, None, None).unwrap();
        assert!(changelog.contains(note), "{changelog}");

        let github = github_notes(&release, None);
        let note_at = github.find(note).unwrap_or_else(|| panic!("{github}"));
        assert!(
            note_at < github.find("**Full Changelog**").unwrap(),
            "{github}"
        );

        let direct =
            generate_release_notes(&release_with(vec![], Some(&["a"])), None, None).unwrap();
        assert!(
            direct.contains("Released because its dependency `a` was updated."),
            "{direct}"
        );
        assert!(
            !generate_release_notes(&release_with(vec![feat_commit()], None), None, None)
                .unwrap()
                .contains("Released because")
        );
    }

    /// Pins offline rendering: conventional grouping and a compare link built from the real tag names.
    #[test]
    fn grouped_template_renders_grouping_and_compare_link_offline() {
        fn commit(msg: &str) -> ConventionalCommit {
            ConventionalCommit {
                hash: "0000000".into(),
                oid: None,
                commit_type: String::new(),
                scope: None,
                description: String::new(),
                body: None,
                breaking: false,
                bump: BumpLevel::None,
                revert: None,
                raw_message: msg.into(),
                files_changed: vec![],
            }
        }
        let release = PackageRelease {
            package_name: "pkg".into(),
            current_version: semver::Version::new(1, 0, 0),
            next_version: semver::Version::new(1, 1, 0),
            bump: BumpLevel::Minor,
            commits: vec![commit("feat: add a thing"), commit("fix: fix a thing")],
            is_root: false,
            propagated_from: None,
        };

        let cliff = enriched_release(
            &release,
            None,
            "https://github.com/o/r",
            "pkg/v1.1.0",
            "pkg/v1.0.0",
            None,
        );
        let notes = render_offline(cliff);

        assert!(
            notes.contains("Add a thing"),
            "missing feature commit:\n{notes}"
        );
        assert!(
            notes.contains("Fix a thing"),
            "missing fix commit:\n{notes}"
        );
        assert!(
            notes.contains("### "),
            "no grouped section heading:\n{notes}"
        );
        assert!(
            notes.contains("https://github.com/o/r/compare/pkg/v1.0.0...pkg/v1.1.0"),
            "compare link missing or wrong:\n{notes}"
        );
    }

    fn contributor(
        name: Option<&str>,
        first_time: bool,
    ) -> git_cliff_core::contributor::RemoteContributor {
        git_cliff_core::contributor::RemoteContributor {
            username: name.map(String::from),
            pr_title: None,
            pr_number: None,
            pr_labels: vec![],
            is_first_time: first_time,
        }
    }

    fn commit_by(id: &str, msg: &str, username: Option<&str>) -> CliffCommit<'static> {
        let mut commit = CliffCommit::new(id.into(), msg.into());
        commit.remote = Some(contributor(username, false));
        commit
    }

    fn render_offline(release: CliffRelease) -> String {
        render_changelog(GITHUB_CLIFF_CONFIG.clone(), release).unwrap()
    }

    /// Offline render of the github template for the given commits and
    /// GitHub-reported contributors.
    fn render_github_notes(
        commits: Vec<CliffCommit<'static>>,
        contributors: Vec<git_cliff_core::contributor::RemoteContributor>,
    ) -> String {
        render_offline(CliffRelease {
            version: Some("1.1.0".into()),
            commits,
            timestamp: Some(0),
            github: git_cliff_core::remote::RemoteReleaseMetadata { contributors },
            extra: Some(serde_json::json!({
                "repo_url": "https://github.com/o/r",
                "tag": "pkg/v1.1.0",
                "previous_tag": "pkg/v1.0.0",
            })),
            ..Default::default()
        })
    }

    /// The github template lists contributors plus a New Contributors highlight, dropping unlinked authors (`username = None`).
    #[test]
    fn contributors_and_new_contributors_render() {
        let notes = render_github_notes(
            vec![
                commit_by("aaaaaaa", "feat: a thing", Some("alice")),
                commit_by("bbbbbbb", "fix: b thing", Some("bob")),
                commit_by("ccccccc", "fix: c thing", None), // unlinked author — must be dropped
            ],
            vec![
                contributor(Some("alice"), true),
                contributor(Some("bob"), false),
                contributor(None, true),
            ],
        );

        assert!(notes.contains("### 🎉 New Contributors"), "{notes}");
        assert!(
            notes.contains("@alice made their first contribution"),
            "{notes}"
        );
        assert!(notes.contains("### 👥 Contributors"), "{notes}");
        assert!(notes.contains("- @alice"), "{notes}");
        assert!(notes.contains("- @bob"), "{notes}");
        assert!(!notes.contains("- @\n"), "unlinked author leaked:\n{notes}");
        assert!(
            !notes.contains("- @ made"),
            "unlinked first-timer leaked:\n{notes}"
        );
    }

    /// Authors whose only commits are skipped by the commit parsers (e.g. dependabot's
    /// `chore(deps)` bumps) must not be credited — in either contributor section.
    #[test]
    fn skipped_commit_authors_are_not_credited() {
        let notes = render_github_notes(
            vec![
                commit_by("aaaaaaa", "feat: a thing", Some("alice")),
                commit_by(
                    "bbbbbbb",
                    "chore(deps-dev): bump foo from 1.0.0 to 2.0.0",
                    Some("dependabot[bot]"),
                ),
            ],
            vec![
                contributor(Some("alice"), false),
                // first_time too, so the New Contributors highlight is covered as well
                contributor(Some("dependabot[bot]"), true),
            ],
        );

        assert!(notes.contains("- @alice"), "{notes}");
        assert!(
            !notes.to_lowercase().contains("bump foo"),
            "skipped commit rendered:\n{notes}"
        );
        assert!(
            !notes.contains("dependabot"),
            "author of skipped commit credited:\n{notes}"
        );
    }

    /// A git-style `Revert "..."` commit must survive git-cliff's
    /// filter_unconventional and render under the default "◀️ Revert" group.
    #[test]
    fn git_style_revert_renders_under_revert_heading() {
        let msg = "Revert \"feat: add a thing\"\n\nThis reverts commit aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.";
        let commit = crate::commit::parse_conventional_commit("00000000", msg).unwrap();
        let release = PackageRelease {
            package_name: "pkg".into(),
            current_version: semver::Version::new(1, 1, 0),
            next_version: semver::Version::new(1, 1, 1),
            bump: BumpLevel::Patch,
            commits: vec![commit],
            is_root: false,
            propagated_from: None,
        };

        let cliff = enriched_release(
            &release,
            None,
            "https://github.com/o/r",
            "pkg/v1.1.1",
            "pkg/v1.1.0",
            None,
        );
        let notes = render_offline(cliff);

        assert!(
            notes.contains("◀️ Revert"),
            "missing revert heading:\n{notes}"
        );
        assert!(
            notes.contains("add a thing"),
            "missing reverted subject:\n{notes}"
        );
    }

    /// A custom template overrides the default body.
    #[test]
    fn custom_template_overrides_body() {
        let release = PackageRelease {
            package_name: "p".into(),
            current_version: semver::Version::new(1, 0, 0),
            next_version: semver::Version::new(1, 1, 0),
            bump: BumpLevel::Minor,
            commits: vec![],
            is_root: false,
            propagated_from: None,
        };
        let notes = generate_release_notes(&release, Some("CUSTOM v{{ version }}"), None).unwrap();
        assert!(
            notes.contains("CUSTOM v1.1.0"),
            "custom template not applied:\n{notes}"
        );
    }
}
