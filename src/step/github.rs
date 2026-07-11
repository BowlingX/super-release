use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::{Path, PathBuf};

use super::changelog::generate_release_notes;
use super::{ReleaseContext, Step, StepConfig, StepContext, parse_options};
use crate::github;
use crate::package::Package;
use crate::version::PackageRelease;

/// Options for the github step.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct GithubOptions {
    /// Create the release as a draft.
    #[serde(default)]
    pub draft: bool,

    /// Force the prerelease flag. Defaults to whether the branch is a prerelease.
    #[serde(default)]
    pub prerelease: Option<bool>,

    /// Glob patterns (relative to the repo root) for files to attach as assets.
    #[serde(default)]
    pub assets: Vec<String>,

    /// Release name template. Placeholders: `{name}`, `{version}`, `{tag}`.
    /// Defaults to the tag.
    #[serde(default)]
    pub release_name_template: Option<String>,

    /// GitHub Enterprise API base URL (e.g. `https://ghe.corp/api/v3`).
    #[serde(default)]
    pub github_url: Option<String>,
}

pub struct GithubStep;

impl Step for GithubStep {
    fn name(&self) -> &str {
        "github"
    }

    fn verify(&self, ctx: &StepContext, config: &StepConfig) -> Result<()> {
        let _opts: GithubOptions = parse_options(config)?;
        // A token is only needed when we will actually publish, i.e. when the
        // tool pushes the tags the release attaches to.
        if !ctx.dry_run && ctx.cfg.git.push && github::token().is_none() {
            anyhow::bail!(
                "the github step requires a GITHUB_TOKEN or GH_TOKEN environment variable"
            );
        }
        Ok(())
    }

    fn release(
        &self,
        ctx: &ReleaseContext,
        config: &StepConfig,
        _packages: &[Package],
        releases: &[PackageRelease],
    ) -> Result<()> {
        let opts: GithubOptions = parse_options(config)?;
        if releases.is_empty() {
            return Ok(());
        }

        let plans = releases
            .iter()
            .map(|r| build_plan(ctx, &opts, r))
            .collect::<Result<Vec<_>>>()?;

        if ctx.dry_run {
            for plan in &plans {
                println!(
                    "  [github] Would create release {} ({} asset(s))",
                    plan.tag,
                    plan.assets.len()
                );
            }
            return Ok(());
        }

        // Releases attach to the tag on the remote, which only exists once the
        // tool has pushed it.
        if !ctx.cfg.git.push {
            println!("  [github] git.push is disabled — skipping (releases attach to pushed tags)");
            return Ok(());
        }

        let token = github::token().context("github step requires a GITHUB_TOKEN or GH_TOKEN")?;
        let gh_repo = github::detect_repo(ctx.repo, &ctx.cfg.git.remote)?;
        let base_uri = opts.github_url.clone().or_else(|| gh_repo.api_base_uri());

        let results = github::publish_releases(&token, base_uri.as_deref(), &gh_repo, &plans)?;
        for (tag, action) in results {
            println!("  [github] {} release {}", action.verb(), tag);
        }
        Ok(())
    }
}

fn build_plan(
    ctx: &ReleaseContext,
    opts: &GithubOptions,
    release: &PackageRelease,
) -> Result<github::ReleasePlan> {
    let tag = ctx.cfg.format_tag(
        &release.package_name,
        &release.next_version,
        release.is_root,
    );
    let name = render_release_name(
        opts.release_name_template.as_deref(),
        &release.package_name,
        &release.next_version.to_string(),
        &tag,
    );
    let prerelease = opts
        .prerelease
        .unwrap_or_else(|| ctx.branch.prerelease.is_some());
    let body = generate_release_notes(release)?;
    let assets = resolve_assets(ctx.repo_root, &opts.assets)?;

    Ok(github::ReleasePlan {
        tag,
        name,
        body,
        draft: opts.draft,
        prerelease,
        assets,
    })
}

fn render_release_name(template: Option<&str>, name: &str, version: &str, tag: &str) -> String {
    match template {
        None => tag.to_string(),
        Some(t) => t
            .replace("{name}", name)
            .replace("{version}", version)
            .replace("{tag}", tag),
    }
}

/// Expand asset glob patterns (relative to the repo root) into a deduplicated
/// list of files. Patterns matching nothing are warned about, not fatal.
fn resolve_assets(repo_root: &Path, patterns: &[String]) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for pattern in patterns {
        let joined = repo_root.join(pattern);
        let full = joined
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("asset pattern is not valid UTF-8: {}", pattern))?;
        let mut matched = false;
        for entry in glob::glob(full)
            .map_err(|e| anyhow::anyhow!("invalid asset glob '{}': {}", pattern, e))?
        {
            let path =
                entry.map_err(|e| anyhow::anyhow!("asset glob error for '{}': {}", pattern, e))?;
            if path.is_file() {
                out.push(path);
                matched = true;
            }
        }
        if !matched {
            eprintln!(
                "  [github] Warning: asset pattern matched no files: {}",
                pattern
            );
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_name_defaults_to_tag() {
        assert_eq!(
            render_release_name(None, "pkg", "1.2.3", "pkg/v1.2.3"),
            "pkg/v1.2.3"
        );
    }

    #[test]
    fn release_name_template_substitutes() {
        assert_eq!(
            render_release_name(Some("{name} {version}"), "pkg", "1.2.3", "pkg/v1.2.3"),
            "pkg 1.2.3"
        );
        assert_eq!(
            render_release_name(Some("Release {tag}"), "pkg", "1.2.3", "pkg/v1.2.3"),
            "Release pkg/v1.2.3"
        );
    }

    #[test]
    fn resolve_assets_expands_globs_and_dedupes() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("a.tgz"), b"x").unwrap();
        std::fs::write(root.join("b.tgz"), b"y").unwrap();
        std::fs::create_dir(root.join("sub")).unwrap();

        let assets = resolve_assets(root, &["*.tgz".into(), "a.tgz".into()]).unwrap();
        let names: Vec<_> = assets
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap().to_string())
            .collect();
        assert_eq!(names, vec!["a.tgz", "b.tgz"]); // sorted + deduped, directories excluded
    }
}
