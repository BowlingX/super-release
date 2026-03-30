use anyhow::Result;

use super::{Plugin, PluginConfig, PluginContext};
use crate::git;
use crate::package::Package;
use crate::version::PackageRelease;

pub struct GitTagPlugin;

impl Plugin for GitTagPlugin {
    fn name(&self) -> &str {
        "git-tag"
    }

    fn publish(
        &self,
        ctx: &PluginContext,
        _config: &PluginConfig,
        _packages: &[Package],
        releases: &[PackageRelease],
    ) -> Result<()> {
        let repo = git::open_repo(ctx.repo_root)?;

        for release in releases {
            let tag_name = ctx.config.format_tag(&release.package_name, &release.next_version);
            let message = format!(
                "Release {} v{}",
                release.package_name, release.next_version
            );

            if ctx.dry_run {
                println!("  [git-tag] Would create tag: {}", tag_name);
                continue;
            }

            git::create_tag(&repo, &tag_name, &message)?;
            println!("  [git-tag] Created tag: {}", tag_name);
        }

        Ok(())
    }
}
