use anyhow::Result;
use chrono::Local;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use super::{Plugin, PluginConfig, PluginContext};
use crate::commit::ConventionalCommit;
use crate::package::Package;
use crate::version::PackageRelease;

pub struct ChangelogPlugin;

impl Plugin for ChangelogPlugin {
    fn name(&self) -> &str {
        "changelog"
    }

    fn prepare(
        &self,
        ctx: &PluginContext,
        _config: &PluginConfig,
        _packages: &[Package],
        releases: &[PackageRelease],
    ) -> Result<()> {
        for release in releases {
            let pkg_dir = _packages
                .iter()
                .find(|p| p.name == release.package_name)
                .map(|p| ctx.repo_root.join(&p.path))
                .unwrap_or_else(|| ctx.repo_root.to_path_buf());

            let changelog_path = pkg_dir.join("CHANGELOG.md");

            if ctx.dry_run {
                let notes = generate_release_notes(release);
                println!(
                    "  [changelog] Would update {} with:\n{}",
                    changelog_path.display(),
                    textwrap(&notes, "    ")
                );
                continue;
            }

            let notes = generate_release_notes(release);
            update_changelog(&changelog_path, &notes)?;
            println!(
                "  [changelog] Updated {}",
                changelog_path.display()
            );
        }
        Ok(())
    }
}

/// Generate markdown release notes for a package release.
pub fn generate_release_notes(release: &PackageRelease) -> String {
    let date = Local::now().format("%Y-%m-%d");
    let mut out = format!("## {} ({})\n\n", release.next_version, date);

    // Group commits by type
    let mut grouped: BTreeMap<&str, Vec<&ConventionalCommit>> = BTreeMap::new();
    for commit in &release.commits {
        let category = match commit.commit_type.as_str() {
            "feat" => "Features",
            "fix" => "Bug Fixes",
            "perf" => "Performance Improvements",
            "revert" => "Reverts",
            "docs" => "Documentation",
            "refactor" => "Code Refactoring",
            "test" => "Tests",
            "build" | "ci" => "Build System",
            _ => "Other Changes",
        };
        grouped.entry(category).or_default().push(commit);
    }

    // Breaking changes section
    let breaking: Vec<&ConventionalCommit> = release.commits.iter().filter(|c| c.breaking).collect();
    if !breaking.is_empty() {
        out.push_str("### BREAKING CHANGES\n\n");
        for commit in &breaking {
            out.push_str(&format!("- **{}**: {} ({})\n", commit_scope(commit), commit.description, commit.hash));
        }
        out.push('\n');
    }

    // Regular sections
    for (category, commits) in &grouped {
        out.push_str(&format!("### {}\n\n", category));
        for commit in commits {
            let scope = commit_scope(commit);
            if scope.is_empty() {
                out.push_str(&format!("- {} ({})\n", commit.description, commit.hash));
            } else {
                out.push_str(&format!("- **{}**: {} ({})\n", scope, commit.description, commit.hash));
            }
        }
        out.push('\n');
    }

    out
}

fn commit_scope(commit: &ConventionalCommit) -> String {
    commit.scope.clone().unwrap_or_default()
}

/// Update or create a CHANGELOG.md file, prepending new content.
fn update_changelog(path: &Path, new_content: &str) -> Result<()> {
    let existing = if path.exists() {
        fs::read_to_string(path)?
    } else {
        String::new()
    };

    let header = "# Changelog\n\n";
    let body = if existing.starts_with("# Changelog") {
        // Strip the existing header and prepend new content after it
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

fn textwrap(text: &str, indent: &str) -> String {
    text.lines()
        .map(|l| format!("{}{}", indent, l))
        .collect::<Vec<_>>()
        .join("\n")
}
