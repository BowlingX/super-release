use anyhow::Result;
use console::style;
use rayon::prelude::*;
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use git_cliff_core::changelog::Changelog;
use git_cliff_core::commit::Commit as CliffCommit;
use git_cliff_core::config::Config as CliffConfig;
use git_cliff_core::release::Release as CliffRelease;

use super::{Step, StepConfig, StepContext, parse_options};
use crate::commit::ConventionalCommit;
use crate::package::Package;
use crate::version::PackageRelease;

static CLIFF_CONFIG: LazyLock<CliffConfig> =
    LazyLock::new(|| "".parse().expect("Failed to load git-cliff default config"));

/// A git-cliff body template: the default conventional grouping (feat/fix/…
/// sections), plus GitHub attribution (`by @author in #PR`), "New Contributors"
/// and full "Contributors" lists, and a "Full Changelog" link. The compare link
/// and PR links use `extra.repo_url` / `extra.tag` / `extra.previous_tag` so they
/// point at the real tag names, not the bare version. Embedded at compile time
/// so it still ships in the distributed binary.
const GITHUB_GROUPED_BODY: &str = include_str!("../../templates/github-release-body.tera");

/// The config for GitHub-enriched notes: the default git-cliff config (grouping
/// via commit parsers) with our [`GITHUB_GROUPED_BODY`] body. Parsed once; the
/// remote/token is set on the clone per release.
static GITHUB_CLIFF_CONFIG: LazyLock<CliffConfig> = LazyLock::new(|| {
    let mut config: CliffConfig = "".parse().expect("Failed to load git-cliff default config");
    config.changelog.body = GITHUB_GROUPED_BODY.to_string();
    config
});

/// Options for the changelog step.
#[derive(Debug, Clone, Deserialize)]
pub struct ChangelogOptions {
    /// Output filename (default: "CHANGELOG.md")
    #[serde(default = "default_filename")]
    pub filename: String,

    /// Max lines to show in dry-run preview (default: 20)
    #[serde(default = "default_preview_lines")]
    pub preview_lines: usize,

    /// Inline git-cliff body template overriding the default (grouped) format.
    #[serde(default)]
    pub template: Option<String>,

    /// Path (relative to the repo root) to a git-cliff body template file.
    /// Takes precedence over `template`.
    #[serde(default)]
    pub template_file: Option<String>,
}

impl Default for ChangelogOptions {
    fn default() -> Self {
        Self {
            filename: default_filename(),
            preview_lines: default_preview_lines(),
            template: None,
            template_file: None,
        }
    }
}

fn default_filename() -> String {
    "CHANGELOG.md".into()
}

fn default_preview_lines() -> usize {
    20
}

pub struct ChangelogStep;

struct PreparedChangelog {
    pkg_name: String,
    path: PathBuf,
    existing: String,
    /// `None` when `existing` already contains this release's section.
    notes: Option<String>,
    next_version: String,
}

impl Step for ChangelogStep {
    fn name(&self) -> &str {
        "changelog"
    }

    fn prepare(
        &self,
        ctx: &StepContext,
        config: &StepConfig,
        packages: &[Package],
        releases: &[PackageRelease],
    ) -> Result<Vec<PathBuf>> {
        let opts: ChangelogOptions = parse_options(config)?;
        // Resolve the custom template once (reads a file) before fanning out.
        let template = super::resolve_template(
            ctx.repo_root,
            opts.template.as_deref(),
            opts.template_file.as_deref(),
        )?;

        // Generate changelogs per package in parallel
        let results: Vec<PreparedChangelog> = releases
            .par_iter()
            .map(|release| {
                let pkg_dir = packages
                    .iter()
                    .find(|p| p.name == release.package_name)
                    .map(|p| ctx.repo_root.join(&p.path))
                    .unwrap_or_else(|| ctx.repo_root.to_path_buf());

                let path = pkg_dir.join(&opts.filename);
                let existing = read_changelog(&path)?;
                let next_version = release.next_version.to_string();

                // Don't duplicate sections written by a concurrent release.
                let notes = if changelog_contains_version(&existing, &next_version) {
                    None
                } else {
                    Some(generate_release_notes(release, template.as_deref())?)
                };

                Ok(PreparedChangelog {
                    pkg_name: release.package_name.clone(),
                    path,
                    existing,
                    notes,
                    next_version,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        // Write/print results sequentially (filesystem writes + stdout)
        for prepared in &results {
            let Some(notes) = &prepared.notes else {
                println!(
                    "  [changelog] {} already contains {}, {} ({})",
                    prepared.path.display(),
                    prepared.next_version,
                    if ctx.dry_run {
                        "would skip"
                    } else {
                        "skipping"
                    },
                    prepared.pkg_name
                );
                continue;
            };

            if ctx.dry_run {
                let total_lines = notes.lines().count();
                let preview: String = notes
                    .lines()
                    .take(opts.preview_lines)
                    .map(|l| format!("    {}", style(l).dim()))
                    .collect::<Vec<_>>()
                    .join("\n");

                println!(
                    "  [changelog] Would update {} ({})",
                    prepared.path.display(),
                    prepared.pkg_name
                );
                println!("{}", preview);
                if total_lines > opts.preview_lines {
                    println!(
                        "    {} (+{} more lines)",
                        style("...").dim(),
                        total_lines - opts.preview_lines
                    );
                }
                continue;
            }

            update_changelog(&prepared.path, notes, &prepared.existing)?;
            println!("  [changelog] Updated {}", prepared.path.display());
        }

        let modified: Vec<PathBuf> = results
            .iter()
            .map(|p| {
                p.path
                    .strip_prefix(ctx.repo_root)
                    .unwrap_or(&p.path)
                    .to_path_buf()
            })
            .collect();

        Ok(modified)
    }
}

/// Convert our commits to git-cliff commits, using the full commit SHA as the
/// id (git-cliff's GitHub integration matches commits to PRs by SHA; the plain
/// changelog template never renders the id, so this is invisible there).
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

/// Generate markdown release notes for a package release using git-cliff.
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
    /// The release's HEAD commit SHA — lets git-cliff decide who is a
    /// first-time contributor accurately (otherwise it over-reports).
    pub head_commit_id: Option<String>,
    /// The repo's web URL (`https://host/owner/repo`) for PR and compare links.
    pub web_url: &'a str,
}

/// Generate release notes enriched with GitHub data — PR links, `@author`
/// mentions, and a "New Contributors" section. Makes GitHub API calls (cached on
/// disk). Uses full commit SHAs so git-cliff can match commits to their pull
/// requests. `tag`/`previous_tag` are the real tag names, used for the compare
/// link (the version alone would produce a wrong URL for prefixed tags).
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

    // Try with the GitHub fetch; if it fails (network, bad token, rate limit),
    // render the same template offline — grouped notes + the compare link still
    // come through, just without contributor attribution.
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

/// Build and render a git-cliff changelog, isolating the fetch panic.
///
/// git-cliff's `Changelog::new` fetches GitHub metadata (when the remote is set)
/// and `.expect()`s on failure (network, bad token, rate limit) — a panic, not
/// an error. Silence the hook, catch the unwind, and surface an `Err` so the
/// caller can fall back to plain notes instead of crashing the release.
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

/// Reads an existing changelog, treating a missing file as empty.
fn read_changelog(path: &Path) -> Result<String> {
    match fs::read_to_string(path) {
        Ok(content) => Ok(content),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e.into()),
    }
}

/// Relies on the git-cliff template rendering headings as `## [<version>]`,
/// pinned by `test_release_notes_heading_format`.
fn changelog_contains_version(existing: &str, version: &str) -> bool {
    existing.contains(&format!("## [{}]", version))
}

fn update_changelog(path: &Path, new_content: &str, existing: &str) -> Result<()> {
    let header = "# Changelog\n\n";
    let body = if existing.starts_with("# Changelog") {
        let rest = existing.strip_prefix("# Changelog").unwrap_or(existing);
        let rest = rest.trim_start_matches('\n');
        format!("{}{}{}", header, new_content, rest)
    } else if existing.is_empty() {
        format!("{}{}", header, new_content)
    } else {
        format!("{}{}{}", header, new_content, existing)
    };

    fs::write(path, body)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commit::BumpLevel;
    use crate::version::PackageRelease;

    /// Pins the heading shape `changelog_contains_version` depends on.
    #[test]
    fn test_release_notes_heading_format() {
        let release = PackageRelease {
            package_name: "my-pkg".into(),
            current_version: semver::Version::new(1, 0, 0),
            next_version: semver::Version::new(1, 1, 0),
            bump: BumpLevel::Minor,
            commits: vec![],
            is_root: false,
            propagated_from: None,
        };

        let notes = generate_release_notes(&release, None).unwrap();
        assert!(
            changelog_contains_version(&notes, "1.1.0"),
            "generated notes no longer match the expected heading format:\n{}",
            notes
        );
        assert!(!changelog_contains_version(&notes, "1.0.0"));
    }

    /// GitHub commit↔PR matching is by full SHA, so the enriched path must use
    /// the full OID as the commit id, not the short display hash.
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

    /// Safety net: git-cliff `.expect()`s on a failed GitHub fetch (a panic).
    /// With a bogus token the fetch fails, and our catch_unwind must turn that
    /// into an `Err` — never an abort/crash. Ignored by default (network).
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
        // Must return Err (caught), not unwind past this call.
        assert!(
            generate_release_notes_with_github(&release, &gh, "p/v1.1.0", "p/v1.0.0", None)
                .is_err()
        );
    }

    /// The grouped GitHub template renders offline (no fetch): conventional
    /// grouping, and a "Full Changelog" link built from the real tag names
    /// (not the bare version). Attribution needs a live fetch, so it's absent
    /// here — but the grouping and compare link are what this pins.
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
        config.remote.offline = true; // no network fetch
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

    /// The github template lists all contributors plus a New Contributors
    /// highlight, dropping unlinked authors (`username = None`). Renders offline
    /// by setting the `github` metadata directly (no fetch).
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
