use anyhow::Result;
use serde::Deserialize;

use super::{parse_options, Plugin, PluginConfig, PluginContext};
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

            if tag_exists(ctx.repo, &tag_name) {
                println!("  [git-tag] Tag already exists: {}, skipping", tag_name);
                continue;
            }

            git::create_tag(ctx.repo, &tag_name, &message)?;
            println!("  [git-tag] Created tag: {}", tag_name);
        }

        if !ctx.dry_run && opts.push && !releases.is_empty() {
            println!("  [git-tag] Pushing tags to {} ...", opts.remote);
            let output = std::process::Command::new("git")
                .arg("push")
                .arg(&opts.remote)
                .arg("--tags")
                .current_dir(ctx.repo_root)
                .output()?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!("git push --tags failed: {}", stderr);
            }
            println!("  [git-tag] Tags pushed");
        }

        Ok(())
    }
}

fn tag_exists(repo: &git2::Repository, tag_name: &str) -> bool {
    repo.find_reference(&format!("refs/tags/{}", tag_name)).is_ok()
}
