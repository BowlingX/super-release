pub mod changelog;
pub mod exec;
pub mod github;
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
    pub cfg: &'a crate::config::Config,
}

/// Context passed to the `release` phase, which runs after the git commit and tags are pushed.
pub struct ReleaseContext<'a> {
    pub repo_root: &'a std::path::Path,
    pub dry_run: bool,
    pub branch: &'a crate::config::BranchContext,
    pub cfg: &'a crate::config::Config,
    pub repo: &'a git2::Repository,
}

/// Steps return the list of files they modified so the core can stage exactly those files.
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

    /// Runs after the git commit and tags are pushed, for publishing to external services that reference the pushed tag.
    fn release(
        &self,
        _ctx: &ReleaseContext,
        _config: &StepConfig,
        _packages: &[Package],
        _releases: &[PackageRelease],
    ) -> Result<()> {
        Ok(())
    }

    /// Whether this step does work in the release phase; drives the "Publishing releases" header.
    fn has_release_phase(&self) -> bool {
        false
    }
}

pub fn create_step(name: &str) -> Option<Box<dyn Step>> {
    match name {
        "changelog" => Some(Box::new(changelog::ChangelogStep)),
        "exec" => Some(Box::new(exec::ExecStep)),
        "github" => Some(Box::new(github::GithubStep)),
        "npm" => Some(Box::new(npm::NpmStep)),
        _ => None,
    }
}

pub fn parse_options<T: serde::de::DeserializeOwned + Default>(config: &StepConfig) -> Result<T> {
    if config.options.is_null() {
        return Ok(T::default());
    }
    serde_json::from_value(config.options.clone())
        .map_err(|e| anyhow::anyhow!("Invalid options for step '{}': {}", config.name, e))
}

/// Resolve a step's custom Tera body template: `template_file` (read relative to repo root) takes precedence over inline `template`, and `None` means the step uses its default.
pub fn resolve_template(
    repo_root: &std::path::Path,
    template: Option<&str>,
    template_file: Option<&str>,
) -> Result<Option<String>> {
    if let Some(path) = template_file {
        let full = repo_root.join(path);
        let body = std::fs::read_to_string(&full)
            .map_err(|e| anyhow::anyhow!("reading template file '{}': {}", full.display(), e))?;
        return Ok(Some(body));
    }
    Ok(template.map(String::from))
}
