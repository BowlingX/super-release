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
    /// GitHub attribution and PR-comment release notes, looked up once per run.
    pub release_meta: Option<&'a crate::pr_notes::ReleaseMetadata>,
}

/// Context passed to the `release` phase, which runs after the git commit and tags are pushed.
pub struct ReleaseContext<'a> {
    pub repo_root: &'a std::path::Path,
    pub dry_run: bool,
    pub branch: &'a crate::config::BranchContext,
    pub cfg: &'a crate::config::Config,
    pub repo: &'a git2::Repository,
    pub release_meta: Option<&'a crate::pr_notes::ReleaseMetadata>,
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
        // A previewed PR controls this path; keep it (and any symlink) inside the repo.
        let resolved = full
            .canonicalize()
            .map_err(|e| anyhow::anyhow!("reading template file '{}': {}", full.display(), e))?;
        if !resolved.starts_with(repo_root.canonicalize()?) {
            anyhow::bail!("template file '{}' is outside the repository", path);
        }
        let body = std::fs::read_to_string(&resolved)
            .map_err(|e| anyhow::anyhow!("reading template file '{}': {}", full.display(), e))?;
        return Ok(Some(body));
    }
    Ok(template.map(String::from))
}

/// [`resolve_template`] for previews, refusing `get_env`: the PR controls the
/// config, and comment-triggered previews hold a write token even for forks.
/// Tera can't call a function by a computed name, so a literal check suffices.
pub fn resolve_preview_template(
    repo_root: &std::path::Path,
    template: Option<&str>,
    template_file: Option<&str>,
) -> Result<Option<String>> {
    let template = resolve_template(repo_root, template, template_file)?;
    if template.as_deref().is_some_and(|t| t.contains("get_env")) {
        anyhow::bail!("the template calls get_env, which previews don't allow");
    }
    Ok(template)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_file_resolves_inside_the_repo_only() {
        let outer = tempfile::tempdir().unwrap();
        let root = outer.path().join("repo");
        std::fs::create_dir_all(root.join("templates")).unwrap();
        std::fs::write(root.join("templates/notes.tera"), "inside").unwrap();
        std::fs::write(outer.path().join("secret"), "outside").unwrap();

        assert_eq!(
            resolve_template(&root, None, Some("templates/notes.tera")).unwrap(),
            Some("inside".into())
        );
        let secret = outer.path().join("secret");
        for escape in ["../secret", secret.to_str().unwrap()] {
            let err = resolve_template(&root, None, Some(escape)).unwrap_err();
            assert!(
                err.to_string().contains("outside the repository"),
                "{escape}: {err}"
            );
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&secret, root.join("link.tera")).unwrap();
            assert!(resolve_template(&root, None, Some("link.tera")).is_err());
        }
    }
}
