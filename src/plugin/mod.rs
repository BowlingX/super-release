pub mod changelog;
pub mod git_commit;
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
pub trait Plugin: Send + Sync {
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
        "git-commit" => Some(Box::new(git_commit::GitCommitPlugin)),
        "git-tag" => Some(Box::new(git_tag::GitTagPlugin)),
        _ => None,
    }
}

/// Helper to deserialize plugin options from the JSON value.
/// Returns the default if options are null or missing.
pub fn parse_options<T: serde::de::DeserializeOwned + Default>(
    config: &PluginConfig,
) -> Result<T> {
    if config.options.is_null() {
        return Ok(T::default());
    }
    serde_json::from_value(config.options.clone())
        .map_err(|e| anyhow::anyhow!("Invalid options for plugin '{}': {}", config.name, e))
}
