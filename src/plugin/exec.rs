use anyhow::{Context, Result};
use serde::Deserialize;
use std::process::Command;

use super::{parse_options, Plugin, PluginConfig, PluginContext};
use crate::config::glob_match;
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
    /// Command to run during the prepare phase (per matching package).
    #[serde(default)]
    pub prepare_cmd: Option<String>,

    /// Command to run during the publish phase (per matching package).
    #[serde(default)]
    pub publish_cmd: Option<String>,

    /// Glob patterns to filter which packages this exec applies to.
    /// If empty, runs for all released packages.
    #[serde(default)]
    pub packages: Vec<String>,
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
    ) -> Result<()> {
        let opts: ExecOptions = parse_options(config)?;
        let Some(cmd_template) = &opts.prepare_cmd else {
            return Ok(());
        };
        run_for_releases(ctx, cmd_template, releases, &opts.packages, "prepare")
    }

    fn publish(
        &self,
        ctx: &PluginContext,
        config: &PluginConfig,
        _packages: &[Package],
        releases: &[PackageRelease],
    ) -> Result<()> {
        let opts: ExecOptions = parse_options(config)?;
        let Some(cmd_template) = &opts.publish_cmd else {
            return Ok(());
        };
        run_for_releases(ctx, cmd_template, releases, &opts.packages, "publish")
    }
}

fn run_for_releases(
    ctx: &PluginContext,
    cmd_template: &str,
    releases: &[PackageRelease],
    filter: &[String],
    phase: &str,
) -> Result<()> {
    let channel = ctx.branch.prerelease.as_deref().unwrap_or("");

    for release in releases {
        if !filter.is_empty()
            && !filter
                .iter()
                .any(|pat| glob_match(pat, &release.package_name))
        {
            continue;
        }

        let cmd = cmd_template
            .replace("{version}", &release.next_version.to_string())
            .replace("{name}", &release.package_name)
            .replace("{channel}", channel);

        if ctx.dry_run {
            println!("  [exec:{}] Would run: {}", phase, cmd);
            continue;
        }

        println!("  [exec:{}] Running: {}", phase, cmd);

        let output = Command::new("sh")
            .arg("-c")
            .arg(&cmd)
            .current_dir(ctx.repo_root)
            .output()
            .with_context(|| format!("Failed to run: {}", cmd))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            anyhow::bail!(
                "exec command failed (exit {}): {}\nstdout: {}\nstderr: {}",
                output.status,
                cmd,
                stdout.trim(),
                stderr.trim()
            );
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        if !stdout.trim().is_empty() {
            for line in stdout.lines().take(3) {
                println!("    {}", line);
            }
        }
    }

    Ok(())
}
