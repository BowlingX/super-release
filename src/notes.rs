//! Release-notes engine: one git-cliff-backed source of markdown notes shared by
//! the changelog, github, and PR-preview steps.

use anyhow::Result;
use std::sync::LazyLock;

use git_cliff_core::changelog::Changelog;
use git_cliff_core::commit::Commit as CliffCommit;
use git_cliff_core::config::Config as CliffConfig;
use git_cliff_core::release::Release as CliffRelease;

use crate::commit::ConventionalCommit;
use crate::version::PackageRelease;

static CLIFF_CONFIG: LazyLock<CliffConfig> =
    LazyLock::new(|| "".parse().expect("Failed to load git-cliff default config"));

/// Compare/PR links use `extra.repo_url`/`extra.tag`/`extra.previous_tag` so they
/// point at real tag names, not the bare version.
const GITHUB_GROUPED_BODY: &str = include_str!("../templates/github-release-body.tera");

/// Parsed once; the remote/token is set on the clone per release.
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
            CliffCommit::new(id, c.raw_message.clone())
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

/// `tag`/`previous_tag` are the real tag names, needed for a correct compare link
/// with prefixed tags.
///
/// Must NOT be called from within a tokio runtime: `Changelog::new` spins up its
/// own runtime and blocks.
pub fn generate_release_notes_with_github(
    release: &PackageRelease,
    gh: &GithubContext,
    tag: &str,
    previous_tag: &str,
    template: Option<&str>,
) -> Result<String> {
    use git_cliff_core::config::Remote;

    let build_config = |offline: bool| {
        let mut config = GITHUB_CLIFF_CONFIG.clone();
        if let Some(body) = template {
            config.changelog.body = body.to_string();
        }
        config.remote.offline = offline;
        if !offline {
            config.remote.github = Remote {
                owner: gh.owner.to_string(),
                repo: gh.repo.to_string(),
                token: Some(secrecy::SecretString::new(gh.token.to_string())),
                is_custom: true,
                api_url: gh.api_url.map(String::from),
                ..Default::default()
            };
        }
        config
    };
    let build_release = || {
        enriched_release(
            release,
            gh.head_commit_id.clone(),
            gh.web_url,
            tag,
            previous_tag,
        )
    };

    // On fetch failure, re-render offline so grouped notes and the compare link still come through, minus attribution.
    match render_changelog(build_config(false), build_release()) {
        Ok(notes) => Ok(notes),
        Err(_) => render_changelog(build_config(true), build_release()),
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

/// git-cliff's `Changelog::new` `.expect()`s (panics) on a failed GitHub fetch, so
/// we catch the unwind and return `Err` to let the caller fall back to plain notes.
fn render_changelog(config: CliffConfig, release: CliffRelease) -> Result<String> {
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        Changelog::new(vec![release], config, None)
    }));
    std::panic::set_hook(prev_hook);

    let changelog = built
        .map_err(|_| anyhow::anyhow!("GitHub metadata fetch failed"))?
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
            raw_message: "feat: x".into(),
            files_changed: vec![],
        };
        let commits = [commit];
        let cliff = to_cliff_commits(&commits);
        assert_eq!(cliff[0].id, sha);
    }

    /// Safety net: a bogus-token fetch must become an `Err` via catch_unwind, never a crash.
    #[test]
    #[ignore = "makes a network call to api.github.com"]
    fn github_enrichment_failure_is_caught_not_crashed() {
        let release = PackageRelease {
            package_name: "p".into(),
            current_version: semver::Version::new(1, 0, 0),
            next_version: semver::Version::new(1, 1, 0),
            bump: BumpLevel::Minor,
            commits: vec![],
            is_root: true,
            propagated_from: None,
        };
        let gh = GithubContext {
            owner: "BowlingX",
            repo: "super-release",
            token: "definitely-not-a-valid-token",
            api_url: None,
            head_commit_id: None,
            web_url: "https://github.com/BowlingX/super-release",
        };
        assert!(
            generate_release_notes_with_github(&release, &gh, "p/v1.1.0", "p/v1.0.0", None)
                .is_err()
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

        let mut config = GITHUB_CLIFF_CONFIG.clone();
        config.remote.offline = true;
        let cliff = enriched_release(
            &release,
            None,
            "https://github.com/o/r",
            "pkg/v1.1.0",
            "pkg/v1.0.0",
        );
        let notes = render_changelog(config, cliff).unwrap();

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

    /// The github template lists contributors plus a New Contributors highlight, dropping unlinked authors (`username = None`).
    #[test]
    fn contributors_and_new_contributors_render() {
        use git_cliff_core::contributor::RemoteContributor;
        use git_cliff_core::remote::RemoteReleaseMetadata;

        let contributor = |name: Option<&str>, first_time: bool| RemoteContributor {
            username: name.map(String::from),
            pr_title: None,
            pr_number: None,
            pr_labels: vec![],
            is_first_time: first_time,
        };
        let cliff = CliffRelease {
            version: Some("1.1.0".into()),
            commits: vec![CliffCommit::new("aaaaaaa".into(), "feat: a thing".into())],
            timestamp: Some(0),
            github: RemoteReleaseMetadata {
                contributors: vec![
                    contributor(Some("alice"), true),
                    contributor(Some("bob"), false),
                    contributor(None, false), // unlinked author — must be dropped
                ],
            },
            extra: Some(serde_json::json!({
                "repo_url": "https://github.com/o/r",
                "tag": "pkg/v1.1.0",
                "previous_tag": "pkg/v1.0.0",
            })),
            ..Default::default()
        };

        let mut config = GITHUB_CLIFF_CONFIG.clone();
        config.remote.offline = true;
        let notes = render_changelog(config, cliff).unwrap();

        assert!(notes.contains("### 🎉 New Contributors"), "{notes}");
        assert!(
            notes.contains("@alice made their first contribution"),
            "{notes}"
        );
        assert!(notes.contains("### 👥 Contributors"), "{notes}");
        assert!(notes.contains("- @alice"), "{notes}");
        assert!(notes.contains("- @bob"), "{notes}");
        assert!(!notes.contains("- @\n"), "unlinked author leaked:\n{notes}");
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
