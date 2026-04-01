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
        let results: Vec<(String, String, String)> = releases
            .par_iter()
            .map(|release| {
                let pkg_dir = packages
                    .iter()
                    .find(|p| p.name == release.package_name)
                    .map(|p| ctx.repo_root.join(&p.path))
                    .unwrap_or_else(|| ctx.repo_root.to_path_buf());

                let changelog_path = pkg_dir.join(&opts.filename);
                let notes = generate_release_notes(release)?;
                Ok((
                    release.package_name.clone(),
                    changelog_path.to_string_lossy().to_string(),
                    notes,
                ))
            })
            .collect::<Result<Vec<_>>>()?;

        // Write/print results sequentially (filesystem writes + stdout)
        for (pkg_name, changelog_path, notes) in &results {
            let path = Path::new(changelog_path);

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
                    path.display(),
                    pkg_name
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

            update_changelog(path, notes)?;
            println!("  [changelog] Updated {}", path.display());
        }

        let modified: Vec<PathBuf> = results
            .iter()
            .map(|(_, p, _)| {
                PathBuf::from(p)
                    .strip_prefix(ctx.repo_root)
                    .unwrap_or(&PathBuf::from(p))
                    .to_path_buf()
            })
            .collect();

        Ok(modified)
    }
}

/// Convert our commits to git-cliff commits.
pub fn to_cliff_commits(commits: &[ConventionalCommit]) -> Vec<CliffCommit<'_>> {
    commits
        .iter()
        .map(|c| CliffCommit::new(c.hash.clone(), c.raw_message.clone()))
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

fn update_changelog(path: &Path, new_content: &str) -> Result<()> {
    let existing = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };

    let header = "# Changelog\n\n";
    let body = if existing.starts_with("# Changelog") {
        let rest = existing.strip_prefix("# Changelog").unwrap_or(&existing);
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
