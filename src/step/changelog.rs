use anyhow::Result;
use console::style;
use rayon::prelude::*;
use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};

use super::{Step, StepConfig, StepContext, parse_options};
use crate::notes::generate_release_notes;
use crate::package::Package;
use crate::version::PackageRelease;

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

    /// Pins the heading shape `changelog_contains_version` depends on: the notes
    /// engine must keep rendering `## [<version>]` for the step's dedup to work.
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
}
