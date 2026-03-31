use anyhow::Result;
use rayon::prelude::*;
use serde::Deserialize;
use std::process::Command;

use super::{Plugin, PluginConfig, PluginContext, parse_options, subprocess};
use crate::package::Package;
use crate::version::PackageRelease;

/// Options for the exec plugin.
///
/// Runs arbitrary shell commands during prepare and/or publish phases.
/// Commands support placeholders:
/// - `{version}` — the next version (e.g. "1.2.0")
/// - `{name}` — the package name
/// - `{channel}` — the prerelease channel (empty for stable)
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ExecOptions {
    /// Command to run during the prepare phase (per package).
    #[serde(default)]
    pub prepare_cmd: Option<String>,

    /// Command to run during the publish phase (per package).
    #[serde(default)]
    pub publish_cmd: Option<String>,

    /// Files to include in the git commit after the command runs.
    /// Supports `{version}` and `{name}` placeholders in paths.
    #[serde(default)]
    pub files: Vec<String>,
}

pub struct ExecPlugin;

impl Plugin for ExecPlugin {
    fn name(&self) -> &str {
        "exec"
    }

    fn prepare(
        &self,
        ctx: &PluginContext,
        config: &PluginConfig,
        _packages: &[Package],
        releases: &[PackageRelease],
    ) -> Result<Vec<std::path::PathBuf>> {
        let opts: ExecOptions = parse_options(config)?;
        let Some(cmd_template) = &opts.prepare_cmd else {
            return Ok(Vec::new());
        };
        run_for_releases(ctx, cmd_template, releases, "prepare")?;
        Ok(resolve_files(&opts.files, releases))
    }

    fn publish(
        &self,
        ctx: &PluginContext,
        config: &PluginConfig,
        _packages: &[Package],
        releases: &[PackageRelease],
    ) -> Result<Vec<std::path::PathBuf>> {
        let opts: ExecOptions = parse_options(config)?;
        let Some(cmd_template) = &opts.publish_cmd else {
            return Ok(Vec::new());
        };
        run_for_releases(ctx, cmd_template, releases, "publish")?;
        Ok(resolve_files(&opts.files, releases))
    }
}

fn resolve_files(patterns: &[String], releases: &[PackageRelease]) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    for release in releases {
        for pattern in patterns {
            let path = pattern
                .replace("{version}", &release.next_version.to_string())
                .replace("{name}", &release.package_name);
            files.push(std::path::PathBuf::from(path));
        }
    }
    files
}

fn run_for_releases(
    ctx: &PluginContext,
    cmd_template: &str,
    releases: &[PackageRelease],
    phase: &str,
) -> Result<()> {
    let channel = ctx.branch.prerelease.as_deref().unwrap_or("");
    let plugin_name = format!("exec:{}", phase);
    let repo_root = ctx.repo_root;
    let dry_run = ctx.dry_run;

    let results: Vec<Result<()>> = releases
        .par_iter()
        .map(|release| {
            let cmd_str = cmd_template
                .replace("{version}", &release.next_version.to_string())
                .replace("{name}", &release.package_name)
                .replace("{channel}", channel);

            if dry_run {
                println!("  [{}] Would run: {}", plugin_name, cmd_str);
                println!(
                    "    {}",
                    console::style(format!("in {}", repo_root.display())).dim()
                );
                return Ok(());
            }

            println!("  [{}] Running: {}", plugin_name, cmd_str);
            println!(
                "    {}",
                console::style(format!("in {}", repo_root.display())).dim()
            );

            let mut cmd = Command::new("sh");
            cmd.arg("-c").arg(&cmd_str).current_dir(repo_root);

            subprocess::run_command(
                cmd,
                &subprocess::RunOptions {
                    label: &cmd_str,
                    plugin_name: &plugin_name,
                    is_recoverable: None,
                },
            )
        })
        .collect();

    for r in results {
        r?;
    }

    Ok(())
}
