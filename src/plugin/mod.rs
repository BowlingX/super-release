pub mod changelog;
pub mod git_tag;
pub mod npm;

use anyhow::Result;

use crate::config::PluginConfig;
use crate::package::Package;
use crate::version::PackageRelease;

/// Context passed to plugins during execution.
pub struct PluginContext<'a> {
    pub repo_root: &'a std::path::Path,
    pub repo: &'a git2::Repository,
    pub dry_run: bool,
    pub config: &'a crate::config::Config,
}

/// Trait that all release plugins must implement.
pub trait Plugin {
    /// Plugin name for matching against config.
    fn name(&self) -> &str;

    /// Validate preconditions (e.g., credentials, tools available).
    fn verify(&self, _ctx: &PluginContext, _config: &PluginConfig) -> Result<()> {
        Ok(())
    }

    /// Prepare the release (e.g., update files, generate changelogs).
    fn prepare(
        &self,
        _ctx: &PluginContext,
        _config: &PluginConfig,
        _packages: &[Package],
        _releases: &[PackageRelease],
    ) -> Result<()> {
        Ok(())
    }

    /// Publish the release (e.g., npm publish, create git tags).
    fn publish(
        &self,
        _ctx: &PluginContext,
        _config: &PluginConfig,
        _packages: &[Package],
        _releases: &[PackageRelease],
    ) -> Result<()> {
        Ok(())
    }
}

/// Create a plugin instance by name.
pub fn create_plugin(name: &str) -> Option<Box<dyn Plugin>> {
    match name {
        "changelog" => Some(Box::new(changelog::ChangelogPlugin)),
        "npm" => Some(Box::new(npm::NpmPlugin)),
        "git-tag" => Some(Box::new(git_tag::GitTagPlugin)),
        _ => None,
    }
}
