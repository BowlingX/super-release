use anyhow::{Context, Result};
use serde::Deserialize;
use std::process::Command;

use super::{parse_options, Plugin, PluginConfig, PluginContext};
use crate::package::Package;
use crate::version::PackageRelease;

/// Options for the git-commit plugin.
#[derive(Debug, Clone, Deserialize)]
pub struct GitCommitOptions {
    /// Commit message template. Supports placeholders:
    /// - `{releases}`: comma-separated list (e.g. "@acme/core@1.1.0, @acme/utils@1.0.1")
    /// - `{summary}`:  one package per line (for the commit body)
    /// - `{count}`:    number of packages released
    #[serde(default = "default_message")]
    pub message: String,

    /// Whether to push the commit to the remote (default: false)
    #[serde(default)]
    pub push: bool,

    /// Git remote to push to (default: "origin")
    #[serde(default = "default_remote")]
    pub remote: String,

    /// Paths to stage. Default: stages all modified/new files (".")
    #[serde(default = "default_paths")]
    pub paths: Vec<String>,
}

impl Default for GitCommitOptions {
    fn default() -> Self {
        Self {
            message: default_message(),
            push: false,
            remote: default_remote(),
            paths: default_paths(),
        }
    }
}

fn default_message() -> String {
    "chore(release): {releases} [skip ci]".into()
}

fn default_remote() -> String {
    "origin".into()
}

fn default_paths() -> Vec<String> {
    vec![".".into()]
}

pub struct GitCommitPlugin;

impl Plugin for GitCommitPlugin {
    fn name(&self) -> &str {
        "git-commit"
    }

    fn publish(
        &self,
        ctx: &PluginContext,
        config: &PluginConfig,
        _packages: &[Package],
        releases: &[PackageRelease],
    ) -> Result<()> {
        let opts: GitCommitOptions = parse_options(config)?;

        let release_list: String = releases
            .iter()
            .map(|r| format!("{}@{}", r.package_name, r.next_version))
            .collect::<Vec<_>>()
            .join(", ");

        let summary: String = releases
            .iter()
            .map(|r| format!("  - {} {} -> {}", r.package_name, r.current_version, r.next_version))
            .collect::<Vec<_>>()
            .join("\n");

        let message = opts
            .message
            .replace("{releases}", &release_list)
            .replace("{summary}", &summary)
            .replace("{count}", &releases.len().to_string());

        if ctx.dry_run {
            println!("  [git-commit] Would stage: {:?}", opts.paths);
            println!("  [git-commit] Would commit: {}", message);
            if opts.push {
                println!("  [git-commit] Would push to {}", opts.remote);
            }
            return Ok(());
        }

        let mut add_cmd = Command::new("git");
        add_cmd.arg("add").current_dir(ctx.repo_root);
        for path in &opts.paths {
            add_cmd.arg(path);
        }
        let output = add_cmd.output().context("Failed to run git add")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("git add failed: {}", stderr);
        }

        // Check if there's anything to commit
        let has_changes = !Command::new("git")
            .args(["diff", "--cached", "--quiet"])
            .current_dir(ctx.repo_root)
            .status()
            .context("Failed to check git status")?
            .success(); // exit code 1 means there are diffs

        if !has_changes {
            println!("  [git-commit] Nothing to commit");
            return Ok(());
        }

        let output = Command::new("git")
            .arg("commit")
            .arg("-m")
            .arg(&message)
            .current_dir(ctx.repo_root)
            .output()
            .context("Failed to run git commit")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("git commit failed: {}", stderr);
        }
        println!("  [git-commit] Committed: {}", message);

        // Push
        if opts.push {
            println!("  [git-commit] Pushing to {} ...", opts.remote);
            let output = Command::new("git")
                .arg("push")
                .arg(&opts.remote)
                .current_dir(ctx.repo_root)
                .output()
                .context("Failed to run git push")?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!("git push failed: {}", stderr);
            }
            println!("  [git-commit] Pushed");
        }

        Ok(())
    }
}
