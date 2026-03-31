pub mod changelog;
pub mod exec;
pub mod npm;
pub mod subprocess;

use anyhow::Result;
use std::path::PathBuf;

use crate::config::PluginConfig;
use crate::package::Package;
use crate::version::PackageRelease;

/// Context passed to plugins during execution.
pub struct PluginContext<'a> {
    pub repo_root: &'a std::path::Path,
    pub dry_run: bool,
    pub branch: &'a crate::config::BranchContext,
}

/// Trait that all release plugins must implement.
/// Plugins return the list of files they modified so the core
/// can stage exactly those files for the git commit.
pub trait Plugin: Send + Sync {
    fn name(&self) -> &str;

    fn verify(&self, _ctx: &PluginContext, _config: &PluginConfig) -> Result<()> {
        Ok(())
    }

    /// Prepare the release. Returns paths of files modified (relative to repo root).
    fn prepare(
        &self,
        _ctx: &PluginContext,
        _config: &PluginConfig,
        _packages: &[Package],
        _releases: &[PackageRelease],
    ) -> Result<Vec<PathBuf>> {
        Ok(Vec::new())
    }

    /// Publish the release. Returns paths of files modified (relative to repo root).
    fn publish(
        &self,
        _ctx: &PluginContext,
        _config: &PluginConfig,
        _packages: &[Package],
        _releases: &[PackageRelease],
    ) -> Result<Vec<PathBuf>> {
        Ok(Vec::new())
    }
}

/// Create a plugin instance by name.
pub fn create_plugin(name: &str) -> Option<Box<dyn Plugin>> {
    match name {
        "changelog" => Some(Box::new(changelog::ChangelogPlugin)),
        "exec" => Some(Box::new(exec::ExecPlugin)),
        "npm" => Some(Box::new(npm::NpmPlugin)),
        _ => None,
    }
}

/// Helper to deserialize plugin options from the JSON value.
pub fn parse_options<T: serde::de::DeserializeOwned + Default>(
    config: &PluginConfig,
) -> Result<T> {
    if config.options.is_null() {
        return Ok(T::default());
    }
    serde_json::from_value(config.options.clone())
        .map_err(|e| anyhow::anyhow!("Invalid options for plugin '{}': {}", config.name, e))
}
