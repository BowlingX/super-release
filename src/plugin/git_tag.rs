use anyhow::Result;
use serde::Deserialize;

use super::{parse_options, subprocess, Plugin, PluginConfig, PluginContext};
use crate::git;
use crate::package::Package;
use crate::version::PackageRelease;

/// Options for the git-tag plugin.
#[derive(Debug, Clone, Deserialize)]
pub struct GitTagOptions {
    /// Whether to push tags to the remote after creation (default: false)
    #[serde(default)]
    pub push: bool,

    /// Git remote to push to (default: "origin")
    #[serde(default = "default_remote")]
    pub remote: String,
}

impl Default for GitTagOptions {
    fn default() -> Self {
        Self {
            push: false,
            remote: default_remote(),
        }
    }
}

fn default_remote() -> String {
    "origin".into()
}

pub struct GitTagPlugin;

impl Plugin for GitTagPlugin {
    fn name(&self) -> &str {
        "git-tag"
    }

    fn publish(
        &self,
        ctx: &PluginContext,
        config: &PluginConfig,
        _packages: &[Package],
        releases: &[PackageRelease],
    ) -> Result<()> {
        let opts: GitTagOptions = parse_options(config)?;
        let mut created_tags = Vec::new();

        for release in releases {
            let tag_name = ctx.config.format_tag(
                &release.package_name,
                &release.next_version,
                release.is_root,
            );
            let message = format!(
                "Release {} v{}",
                release.package_name, release.next_version
            );

            if ctx.dry_run {
                let push_info = if opts.push {
                    format!(" (push to {})", opts.remote)
                } else {
                    String::new()
                };
                println!("  [git-tag] Would create tag: {}{}", tag_name, push_info);
                continue;
            }

            if git::tag_to_oid(ctx.repo, &tag_name)?.is_some() {
                println!("  [git-tag] Tag already exists: {}, skipping", tag_name);
                continue;
            }

            git::create_tag(ctx.repo, &tag_name, &message)?;
            created_tags.push(tag_name);
            println!("  [git-tag] Created tag: {}", created_tags.last().unwrap());
        }

        if !ctx.dry_run && opts.push && !created_tags.is_empty() {
            let mut cmd = std::process::Command::new("git");
            cmd.arg("push").arg(&opts.remote).current_dir(ctx.repo_root);
            for tag in &created_tags {
                cmd.arg(tag);
            }
            println!("  [git-tag] Running: {}", subprocess::format_command(&cmd));
            let output = cmd.output()?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!("git push failed: {}", stderr);
            }
            println!("  [git-tag] Tags pushed");
        }

        Ok(())
    }
}

