use anyhow::Result;
use console::style;
use std::fs;
use std::path::Path;
use std::sync::LazyLock;

use git_cliff_core::changelog::Changelog;
use git_cliff_core::commit::Commit as CliffCommit;
use git_cliff_core::config::Config as CliffConfig;
use git_cliff_core::release::Release as CliffRelease;

use super::{Plugin, PluginConfig, PluginContext};
use crate::commit::ConventionalCommit;
use crate::package::Package;
use crate::version::PackageRelease;

static CLIFF_CONFIG: LazyLock<CliffConfig> = LazyLock::new(|| {
    "".parse().expect("Failed to load git-cliff default config")
});

/// Max lines to show in dry-run changelog preview.
const DRY_RUN_MAX_LINES: usize = 20;

pub struct ChangelogPlugin;

impl Plugin for ChangelogPlugin {
    fn name(&self) -> &str {
        "changelog"
    }

    fn prepare(
        &self,
        ctx: &PluginContext,
        _config: &PluginConfig,
        packages: &[Package],
        releases: &[PackageRelease],
    ) -> Result<()> {
        for release in releases {
            let pkg_dir = packages
                .iter()
                .find(|p| p.name == release.package_name)
                .map(|p| ctx.repo_root.join(&p.path))
                .unwrap_or_else(|| ctx.repo_root.to_path_buf());

            let changelog_path = pkg_dir.join("CHANGELOG.md");
            let notes = generate_release_notes(release)?;

            if ctx.dry_run {
                let total_lines = notes.lines().count();
                let preview: String = notes
                    .lines()
                    .take(DRY_RUN_MAX_LINES)
                    .map(|l| format!("    {}", style(l).dim()))
                    .collect::<Vec<_>>()
                    .join("\n");

                println!("  [changelog] Would update {}", changelog_path.display());
                println!("{}", preview);
                if total_lines > DRY_RUN_MAX_LINES {
                    println!(
                        "    {} (+{} more lines)",
                        style("...").dim(),
                        total_lines - DRY_RUN_MAX_LINES
                    );
                }
                continue;
            }

            update_changelog(&changelog_path, &notes)?;
            println!("  [changelog] Updated {}", changelog_path.display());
        }
        Ok(())
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
    let existing = if path.exists() {
        fs::read_to_string(path)?
    } else {
        String::new()
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
