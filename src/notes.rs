//! Release-notes engine: one git-cliff-backed source of markdown notes shared by
//! the changelog, github, and PR-preview steps.

use anyhow::Result;
use std::sync::LazyLock;

use git_cliff_core::changelog::Changelog;
use git_cliff_core::commit::Commit as CliffCommit;
use git_cliff_core::config::{Config as CliffConfig, Remote};
use git_cliff_core::release::Release as CliffRelease;
use git_cliff_core::remote::RemoteMetadata;
use git_cliff_core::remote::github::GitHubClient;

use crate::commit::ConventionalCommit;
use crate::version::PackageRelease;

static CLIFF_CONFIG: LazyLock<CliffConfig> =
    LazyLock::new(|| "".parse().expect("Failed to load git-cliff default config"));

/// Compare/PR links use `extra.repo_url`/`extra.tag`/`extra.previous_tag` so they
/// point at real tag names, not the bare version. Contributor credits are derived
/// from the rendered commits, not `github.contributors`, which also counts commits
/// skipped by the commit parsers (e.g. `chore(deps)` bumps).
const GITHUB_GROUPED_BODY: &str = include_str!("../templates/github-release-body.tera");

/// Parsed once; the GitHub remote is set on the clone per release.
static GITHUB_CLIFF_CONFIG: LazyLock<CliffConfig> = LazyLock::new(|| {
    let mut config: CliffConfig = "".parse().expect("Failed to load git-cliff default config");
    config.changelog.body = GITHUB_GROUPED_BODY.to_string();
    config
});

/// Uses the full commit SHA as the id because git-cliff matches commits to PRs by SHA.
pub fn to_cliff_commits(commits: &[ConventionalCommit]) -> Vec<CliffCommit<'_>> {
    commits
        .iter()
        .map(|c| {
            let id = c
                .oid
                .map(|o| o.to_string())
                .unwrap_or_else(|| c.hash.clone());
            // git-cliff's default config filters unconventional subjects, which
            // would silently drop git-style reverts from rendered notes.
            CliffCommit::new(id, c.normalized_message())
        })
        .collect()
}

/// `template` overrides the default (grouped conventional) body.
pub fn generate_release_notes(release: &PackageRelease, template: Option<&str>) -> Result<String> {
    let mut config = CLIFF_CONFIG.clone();
    if let Some(body) = template {
        config.changelog.body = body.to_string();
    }
    render_changelog(config, plain_release(release))
}

/// Build a git-cliff release from ours, for offline (no-remote) rendering.
fn plain_release(release: &PackageRelease) -> CliffRelease<'_> {
    CliffRelease {
        version: Some(release.next_version.to_string()),
        commits: to_cliff_commits(&release.commits),
        timestamp: Some(chrono::Local::now().timestamp()),
        previous: Some(Box::new(CliffRelease {
            version: Some(release.current_version.to_string()),
            ..Default::default()
        })),
        ..Default::default()
    }
}

/// Where to fetch GitHub metadata (contributors, PR links) from.
pub struct GithubContext<'a> {
    pub owner: &'a str,
    pub repo: &'a str,
    pub token: &'a str,
    /// GitHub Enterprise API base URL, if any.
    pub api_url: Option<&'a str>,
    /// The release's HEAD commit SHA — lets git-cliff pick first-time contributors accurately (otherwise it over-reports).
    pub head_commit_id: Option<String>,
    /// The repo's web URL (`https://host/owner/repo`) for PR and compare links.
    pub web_url: &'a str,
}

impl GithubContext<'_> {
    fn remote(&self) -> Remote {
        Remote {
            owner: self.owner.to_string(),
            repo: self.repo.to_string(),
            token: Some(secrecy::SecretString::new(self.token.to_string())),
            api_url: self.api_url.map(String::from),
            ..Default::default()
        }
    }
}

/// Fetches the repo's commits and closed pull requests once, so every release in a
/// run is attributed from the same data instead of each refetching both lists.
///
/// Must NOT be called from within a tokio runtime: it builds its own and blocks.
pub fn fetch_github_metadata(gh: &GithubContext) -> Result<RemoteMetadata> {
    let client = GitHubClient::try_from(gh.remote())?;
    Ok(crate::forge::block_on(async {
        tokio::try_join!(client.get_commits(None), client.get_pull_requests())
    })?)
}

/// `tag`/`previous_tag` are the real tag names, needed for a correct compare link
/// with prefixed tags. Without `metadata` the notes keep their grouping and compare
/// link but credit no contributors or PRs.
pub fn generate_release_notes_with_github(
    release: &PackageRelease,
    gh: &GithubContext,
    metadata: Option<&RemoteMetadata>,
    tag: &str,
    previous_tag: &str,
    template: Option<&str>,
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
    );
    if let Some((commits, pull_requests)) = metadata {
        cliff_release.update_github_metadata(commits.clone(), pull_requests.clone())?;
    }
    render_changelog(config, cliff_release)
}

/// Build a git-cliff release from ours, attaching the tag/URL data the template
/// needs via `extra`.
fn enriched_release<'a>(
    release: &'a PackageRelease,
    head_commit_id: Option<String>,
    web_url: &str,
    tag: &str,
    previous_tag: &str,
) -> CliffRelease<'a> {
    CliffRelease {
        version: Some(release.next_version.to_string()),
        commits: to_cliff_commits(&release.commits),
        commit_id: head_commit_id,
        timestamp: Some(chrono::Local::now().timestamp()),
        previous: Some(Box::new(CliffRelease {
            version: Some(release.current_version.to_string()),
            ..Default::default()
        })),
        extra: Some(serde_json::json!({
            "repo_url": web_url,
            "tag": tag,
            "previous_tag": previous_tag,
        })),
        ..Default::default()
    }
}

fn render_changelog(mut config: CliffConfig, release: CliffRelease) -> Result<String> {
    // git-cliff `.expect()`s its own remote fetch, and `GIT_CLIFF__*` env vars can enable one;
    // remote data comes from `fetch_github_metadata` instead.
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

    /// The once-fetched metadata credits the author and PR of a commit matched by full SHA.
    #[test]
    fn injected_github_metadata_credits_author_and_pr() {
        use git_cliff_core::remote::github::{GitHubCommit, GitHubCommitAuthor, GitHubPullRequest};

        let sha = "1234567890abcdef1234567890abcdef12345678";
        let mut commit =
            crate::commit::parse_conventional_commit("12345678", "feat: add a thing").unwrap();
        commit.oid = Some(git2::Oid::from_str(sha).unwrap());
        let release = PackageRelease {
            package_name: "p".into(),
            current_version: semver::Version::new(1, 0, 0),
            next_version: semver::Version::new(1, 1, 0),
            bump: BumpLevel::Minor,
            commits: vec![commit],
            is_root: true,
            propagated_from: None,
        };
        let metadata: RemoteMetadata = (
            vec![Box::new(GitHubCommit {
                sha: sha.into(),
                author: Some(GitHubCommitAuthor {
                    login: Some("octocat".into()),
                }),
                commit: None,
            })],
            vec![Box::new(GitHubPullRequest {
                number: 7,
                title: Some("Add a thing".into()),
                merge_commit_sha: Some(sha.into()),
                labels: vec![],
            })],
        );

        let notes = generate_release_notes_with_github(
            &release,
            &GithubContext {
                owner: "o",
                repo: "r",
                token: "t",
                api_url: None,
                head_commit_id: None,
                web_url: "https://github.com/o/r",
            },
            Some(&metadata),
            "p/v1.1.0",
            "p/v1.0.0",
            None,
        )
        .unwrap();

        assert!(
            notes.contains("by @octocat in [#7](https://github.com/o/r/pull/7)"),
            "author/PR attribution missing:\n{notes}"
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
        let notes = generate_release_notes(&release, Some("CUSTOM v{{ version }}")).unwrap();
        assert!(
            notes.contains("CUSTOM v1.1.0"),
            "custom template not applied:\n{notes}"
        );
    }
}
