use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::changelog::generate_release_notes;
use super::{ReleaseContext, Step, StepConfig, StepContext, parse_options};
use crate::commit::referenced_issues;
use crate::forge::{self, Forge, github::GitHubForge};
use crate::package::Package;
use crate::version::PackageRelease;

/// Marker used to find (and avoid duplicating) our "released" comments.
const RELEASED_MARKER: &str = "<!-- super-release:released -->";

/// Options for the github step.
#[derive(Debug, Clone, Deserialize)]
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

    /// Comment on the PRs/issues a release resolves. Default: true.
    #[serde(default = "default_true")]
    pub comment_on_success: bool,

    /// Success-comment template. Placeholders: `{releases}` (the tags), `{tag}`.
    #[serde(default)]
    pub success_comment: Option<String>,

    /// Labels to add to resolved PRs/issues. Default: `["released"]`.
    #[serde(default = "default_released_labels")]
    pub released_labels: Vec<String>,
}

fn default_true() -> bool {
    true
}

fn default_released_labels() -> Vec<String> {
    vec!["released".into()]
}

impl Default for GithubOptions {
    fn default() -> Self {
        Self {
            draft: false,
            prerelease: None,
            assets: Vec::new(),
            release_name_template: None,
            github_url: None,
            comment_on_success: true,
            success_comment: None,
            released_labels: default_released_labels(),
        }
    }
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
        if !ctx.dry_run && ctx.cfg.git.push && GitHubForge.token().is_none() {
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

        let comments = if opts.comment_on_success {
            build_success_comments(ctx, &opts, releases)
        } else {
            Vec::new()
        };

        if ctx.dry_run {
            for plan in &plans {
                println!(
                    "  [github] Would create release {} ({} asset(s))",
                    plan.tag,
                    plan.assets.len()
                );
            }
            for c in &comments {
                println!("  [github] Would comment on #{}", c.id);
            }
            return Ok(());
        }

        // Releases attach to the tag on the remote, which only exists once the
        // tool has pushed it.
        if !ctx.cfg.git.push {
            println!("  [github] git.push is disabled — skipping (releases attach to pushed tags)");
            return Ok(());
        }

        let forge = GitHubForge;
        let token = forge
            .token()
            .context("github step requires a GITHUB_TOKEN or GH_TOKEN")?;
        let gh_repo = forge.detect_repo(ctx.repo, &ctx.cfg.git.remote)?;
        let base_uri = opts
            .github_url
            .clone()
            .or_else(|| forge.api_base_uri(&gh_repo));

        let results = forge.publish_releases(&token, base_uri.as_deref(), &gh_repo, &plans)?;
        for (tag, action) in results {
            println!("  [github] {} release {}", action.verb(), tag);
        }

        if !comments.is_empty() {
            let n = forge.comment_on_issues(
                &token,
                base_uri.as_deref(),
                &gh_repo,
                RELEASED_MARKER,
                &comments,
            )?;
            if n > 0 {
                println!("  [github] Commented on {} resolved issue(s)/PR(s)", n);
            }
        }
        Ok(())
    }
}

/// Aggregate the issues/PRs each release resolves into one comment per number,
/// mentioning every tag that included it (a PR may touch several packages).
fn build_success_comments(
    ctx: &ReleaseContext,
    opts: &GithubOptions,
    releases: &[PackageRelease],
) -> Vec<forge::IssueComment> {
    let mut id_to_tags: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for release in releases {
        let tag = ctx.cfg.format_tag(
            &release.package_name,
            &release.next_version,
            release.is_root,
        );
        for commit in &release.commits {
            for id in referenced_issues(&commit.raw_message) {
                let tags = id_to_tags.entry(id).or_default();
                if !tags.contains(&tag) {
                    tags.push(tag.clone());
                }
            }
        }
    }

    id_to_tags
        .into_iter()
        .map(|(id, tags)| forge::IssueComment {
            id,
            body: render_success_comment(opts.success_comment.as_deref(), &tags),
            labels: opts.released_labels.clone(),
        })
        .collect()
}

fn render_success_comment(template: Option<&str>, tags: &[String]) -> String {
    let releases = tags
        .iter()
        .map(|t| format!("`{}`", t))
        .collect::<Vec<_>>()
        .join(", ");
    match template {
        Some(t) => t
            .replace("{releases}", &releases)
            .replace("{tag}", tags.first().map(String::as_str).unwrap_or("")),
        None => format!(
            "🎉 This is included in the following release(s): {}",
            releases
        ),
    }
}

fn build_plan(
    ctx: &ReleaseContext,
    opts: &GithubOptions,
    release: &PackageRelease,
) -> Result<forge::ReleasePlan> {
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

    Ok(forge::ReleasePlan {
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

    #[test]
    fn success_comment_default_lists_releases() {
        let body = render_success_comment(None, &["v1.1.0".into(), "core/v2.0.0".into()]);
        assert!(body.contains("`v1.1.0`"));
        assert!(body.contains("`core/v2.0.0`"));
    }

    #[test]
    fn success_comment_template_substitutes() {
        let body =
            render_success_comment(Some("Shipped in {tag} ({releases})"), &["v1.1.0".into()]);
        assert_eq!(body, "Shipped in v1.1.0 (`v1.1.0`)");
    }

    #[test]
    fn options_default_enables_comments_and_released_label() {
        // parse_options returns Default when options are absent; comments must
        // stay on and the label present, unlike a derived Default.
        let opts = GithubOptions::default();
        assert!(opts.comment_on_success);
        assert_eq!(opts.released_labels, vec!["released".to_string()]);
    }
}
