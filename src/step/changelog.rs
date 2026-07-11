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

/// git-cliff's built-in GitHub template + config, parsed once and cloned per
/// enriched release (the remote/token is set on the clone).
static GITHUB_CLIFF_CONFIG: LazyLock<CliffConfig> = LazyLock::new(|| {
    git_cliff_core::embed::BuiltinConfig::parse("github".to_string())
        .expect("Failed to load git-cliff github template")
        .0
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
}

impl Default for ChangelogOptions {
    fn default() -> Self {
        Self {
            filename: default_filename(),
            preview_lines: default_preview_lines(),
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
                    Some(generate_release_notes(release)?)
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
pub fn generate_release_notes(release: &PackageRelease) -> Result<String> {
    let cliff_release = CliffRelease {
        version: Some(release.next_version.to_string()),
        commits: to_cliff_commits(&release.commits),
        timestamp: Some(chrono::Local::now().timestamp()),
        previous: Some(Box::new(CliffRelease {
            version: Some(release.current_version.to_string()),
            ..Default::default()
        })),
        ..Default::default()
    };

    let changelog = Changelog::new(vec![cliff_release], CLIFF_CONFIG.clone(), None)
        .map_err(|e| anyhow::anyhow!("Failed to create changelog: {}", e))?;

    let mut output = Vec::new();
    changelog
        .generate(&mut output)
        .map_err(|e| anyhow::anyhow!("Failed to generate changelog: {}", e))?;

    Ok(String::from_utf8(output)?)
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
}

/// Generate release notes enriched with GitHub data — PR links, `@author`
/// mentions, and a "New Contributors" section — using git-cliff's GitHub
/// template. Makes GitHub API calls (cached on disk). Uses full commit SHAs so
/// git-cliff can match commits to their pull requests.
///
/// Must NOT be called from within a tokio runtime: `Changelog::new` spins up its
/// own runtime and blocks.
pub fn generate_release_notes_with_github(
    release: &PackageRelease,
    gh: &GithubContext,
) -> Result<String> {
    use git_cliff_core::config::Remote;

    // git-cliff's built-in GitHub template renders "What's Changed", PR links,
    // and "New Contributors" — the GitHub-native release style.
    let mut config = GITHUB_CLIFF_CONFIG.clone();
    config.remote.offline = false;
    config.remote.github = Remote {
        owner: gh.owner.to_string(),
        repo: gh.repo.to_string(),
        token: Some(secrecy::SecretString::new(gh.token.to_string())),
        is_custom: true,
        api_url: gh.api_url.map(String::from),
        ..Default::default()
    };

    let cliff_release = CliffRelease {
        version: Some(release.next_version.to_string()),
        commits: to_cliff_commits(&release.commits),
        commit_id: gh.head_commit_id.clone(),
        timestamp: Some(chrono::Local::now().timestamp()),
        previous: Some(Box::new(CliffRelease {
            version: Some(release.current_version.to_string()),
            ..Default::default()
        })),
        ..Default::default()
    };

    // git-cliff's `Changelog::new` fetches GitHub metadata and `.expect()`s on
    // failure (network, bad token, rate limit) — a panic, not an error. Isolate
    // it: silence the hook, catch the unwind, and surface an Err so the caller
    // falls back to plain notes instead of crashing the release.
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        Changelog::new(vec![cliff_release], config, None)
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

        let notes = generate_release_notes(&release).unwrap();
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
        };
        // Must return Err (caught), not unwind past this call.
        assert!(generate_release_notes_with_github(&release, &gh).is_err());
    }
}
