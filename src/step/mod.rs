pub mod changelog;
pub mod exec;
pub mod npm;
pub mod subprocess;

use anyhow::Result;
use std::path::PathBuf;

use crate::config::StepConfig;
use crate::package::Package;
use crate::version::PackageRelease;

/// Context passed to steps during execution.
pub struct StepContext<'a> {
    pub repo_root: &'a std::path::Path,
    pub dry_run: bool,
    pub branch: &'a crate::config::BranchContext,
}

/// Trait that all release steps must implement.
/// Steps return the list of files they modified so the core
/// can stage exactly those files for the git commit.
pub trait Step: Send + Sync {
    fn name(&self) -> &str;

    fn verify(&self, _ctx: &StepContext, _config: &StepConfig) -> Result<()> {
        Ok(())
    }

    /// Prepare the release. Returns paths of files modified (relative to repo root).
    fn prepare(
        &self,
        _ctx: &StepContext,
        _config: &StepConfig,
        _packages: &[Package],
        _releases: &[PackageRelease],
    ) -> Result<Vec<PathBuf>> {
        Ok(Vec::new())
    }

    /// Publish the release. Returns paths of files modified (relative to repo root).
    fn publish(
        &self,
        _ctx: &StepContext,
        _config: &StepConfig,
        _packages: &[Package],
        _releases: &[PackageRelease],
    ) -> Result<Vec<PathBuf>> {
        Ok(Vec::new())
    }
}

/// Create a step instance by name.
pub fn create_step(name: &str) -> Option<Box<dyn Step>> {
    match name {
        "changelog" => Some(Box::new(changelog::ChangelogStep)),
        "exec" => Some(Box::new(exec::ExecStep)),
        "npm" => Some(Box::new(npm::NpmStep)),
        _ => None,
    }
}

/// Helper to deserialize step options from the JSON value.
pub fn parse_options<T: serde::de::DeserializeOwned + Default>(config: &StepConfig) -> Result<T> {
    if config.options.is_null() {
        return Ok(T::default());
    }
    serde_json::from_value(config.options.clone())
        .map_err(|e| anyhow::anyhow!("Invalid options for step '{}': {}", config.name, e))
}
