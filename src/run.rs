//! Release orchestration: the phases `main` drives and the branch/package filtering shared across them.

use anyhow::{Context, Result};
use console::style;

use crate::cli::Cli;
use crate::{config, forge, git, package, preview, step, version};

/// Core git finalize: stage modified files, commit, tag, optionally push.
pub fn finalize_git(
    repo_root: &std::path::Path,
    repo: &git2::Repository,
    cfg: &config::Config,
    releases: &[version::PackageRelease],
    modified_files: &[std::path::PathBuf],
    dry_run: bool,
) -> Result<()> {
    use std::process::Command;

    let release_list: String = releases
        .iter()
        .map(|r| format!("{}@{}", r.package_name, r.next_version))
        .collect::<Vec<_>>()
        .join(", ");

    let summary: String = releases
        .iter()
        .map(|r| {
            format!(
                "  - {} {} -> {}",
                r.package_name, r.current_version, r.next_version
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    let message = cfg
        .git
        .commit_message
        .replace("{releases}", &release_list)
        .replace("{summary}", &summary)
        .replace("{count}", &releases.len().to_string());

    if dry_run {
        printfl!("  [git] Would stage {} file(s)", modified_files.len());
        for f in modified_files {
            printfl!("    {}", style(f.display()).dim());
        }
        printfl!("  [git] Would commit: {}", message);
        for release in releases {
            let tag = cfg.format_tag(
                &release.package_name,
                &release.next_version,
                release.is_root,
            );
            printfl!("  [git] Would create tag: {}", tag);
        }
        if cfg.git.push {
            printfl!("  [git] Would push to {}", cfg.git.remote);
        }
        return Ok(());
    }

    if !modified_files.is_empty() {
        let mut add_cmd = Command::new("git");
        add_cmd.arg("add").current_dir(repo_root);
        for f in modified_files {
            add_cmd.arg(f);
        }
        let output = add_cmd.output().context("Failed to run git add")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("git add failed: {}", stderr);
        }
    }

    let has_staged = !Command::new("git")
        .args(["diff", "--cached", "--quiet"])
        .current_dir(repo_root)
        .status()?
        .success();

    if has_staged {
        let output = Command::new("git")
            .args(["commit", "-m", &message])
            .current_dir(repo_root)
            .output()
            .context("Failed to run git commit")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("git commit failed: {}", stderr);
        }
        printfl!("  [git] Committed: {}", message);
    } else {
        printfl!("  [git] Nothing to commit");
    }

    let mut created_tags: Vec<String> = Vec::new();
    for release in releases {
        let tag_name = cfg.format_tag(
            &release.package_name,
            &release.next_version,
            release.is_root,
        );

        if git::tag_to_oid(repo, &tag_name)?.is_some() {
            printfl!("  [git] Tag already exists: {}, skipping", tag_name);
            continue;
        }

        let tag_message = format!("Release {} v{}", release.package_name, release.next_version);
        git::create_tag(repo, &tag_name, &tag_message)?;
        printfl!("  [git] Created tag: {}", tag_name);
        created_tags.push(tag_name);
    }

    if cfg.git.push {
        // A concurrent release may have already pushed some of these tags, and re-pushing an existing tag fails the entire push.
        let on_remote = git::remote_existing_tags(repo_root, &cfg.git.remote, &created_tags)
            .unwrap_or_else(|e| {
                printfl!(
                    "  [git] Warning: could not check remote tags ({}), pushing all",
                    e
                );
                Default::default()
            });
        created_tags.retain(|tag| {
            let Some(remote_oid) = on_remote.get(tag) else {
                return true;
            };
            let local_oid = git::tag_to_oid(repo, tag).ok().flatten();
            if local_oid.is_some_and(|oid| oid.to_string() == *remote_oid) {
                printfl!("  [git] Tag already on remote: {}, skipping push", tag);
            } else {
                printfl!(
                    "  [git] Tag already on remote (points at a different commit): {}, skipping push",
                    tag
                );
            }
            false
        });

        if has_staged || !created_tags.is_empty() {
            printfl!("  [git] Pushing to {} ...", cfg.git.remote);
            let mut push_cmd = Command::new("git");
            push_cmd.arg("push");
            // All refs or nothing: a lost push race must not leave tags on the remote whose release commit never landed on the branch.
            if cfg.git.atomic {
                push_cmd.arg("--atomic");
            }
            push_cmd
                .arg(&cfg.git.remote)
                .arg("HEAD")
                .current_dir(repo_root);
            for tag in &created_tags {
                push_cmd.arg(tag);
            }
            let output = push_cmd.output()?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!("git push failed: {}", stderr);
            }
            printfl!("  [git] Pushed");
        } else {
            printfl!("  [git] Nothing to push");
        }
    }

    Ok(())
}

/// Print the next version (or current if unchanged) for a package and exit.
pub fn show_next_version(
    packages: &[package::Package],
    releases: &[version::PackageRelease],
    filter: Option<&str>,
) -> Result<()> {
    let target = match filter {
        Some(name) => packages
            .iter()
            .find(|p| p.name == name || config::glob_match(name, &p.name))
            .ok_or_else(|| anyhow::anyhow!("Package '{}' not found", name))?,
        None => {
            if packages.len() == 1 {
                &packages[0]
            } else {
                let root = packages.iter().find(|p| p.is_root);
                root.ok_or_else(|| {
                    anyhow::anyhow!(
                        "Multiple packages found. Use --package to select one: {}",
                        packages
                            .iter()
                            .map(|p| p.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })?
            }
        }
    };

    let version = releases
        .iter()
        .find(|r| r.package_name == target.name)
        .map(|r| r.next_version.to_string())
        .unwrap_or_else(|| target.version.to_string());

    println!("{}", version);
    Ok(())
}

/// Render a release preview for a PR, posting it as a sticky comment or printing the Markdown; makes no changes to the repo.
pub fn run_preview(
    cli: &Cli,
    repo: &git2::Repository,
    repo_root: &std::path::Path,
    cfg: &config::Config,
    packages: &[package::Package],
) -> Result<()> {
    let forge = forge::resolve_forge(repo, &cfg.git.remote);
    let pr_ctx = forge.detect_pr_context();

    let base_branch = cli
        .base
        .clone()
        .or_else(|| pr_ctx.as_ref().and_then(|c| c.base_ref.clone()))
        .or_else(|| {
            std::env::var("GITHUB_BASE_REF")
                .ok()
                .filter(|s| !s.is_empty())
        })
        .or_else(|| cfg.branches.first().map(|b| b.name().to_string()))
        .unwrap_or_else(|| "main".to_string());

    // Fall back to a plain stable context when the base isn't a configured release branch, so the preview still reflects the commit bumps.
    let branch_ctx = config::resolve_named_branch_context(&cfg.branches, &base_branch)?
        .unwrap_or_else(|| config::BranchContext {
            branch_name: base_branch.clone(),
            prerelease: None,
            maintenance: false,
            maintenance_range: None,
            channel: None,
            packages: Vec::new(),
        });

    let mut releases = version::determine_releases(repo, repo_root, packages, cfg, &branch_ctx)?;
    apply_branch_package_filter(&mut releases, &branch_ctx);

    // Only preview notes for packages a `changelog` step would cover on this branch, mirroring the per-step branch + package filtering.
    let notes_packages: std::collections::HashSet<String> = releases
        .iter()
        .filter(|r| changelog_covers(cfg, &base_branch, &r.package_name))
        .map(|r| r.package_name.clone())
        .collect();

    // Best-effort: a broken template path here just falls back to the default.
    let changelog_template = cfg
        .steps
        .iter()
        .find(|s| s.name == "changelog" && step_runs_on_branch(s, &base_branch))
        .and_then(|s| step::parse_options::<step::changelog::ChangelogOptions>(s).ok())
        .and_then(|opts| {
            step::resolve_template(
                repo_root,
                opts.template.as_deref(),
                opts.template_file.as_deref(),
            )
            .ok()
            .flatten()
        });

    let markdown = preview::render_preview_markdown(
        &releases,
        &notes_packages,
        changelog_template.as_deref(),
        cfg,
    );

    let pr_id = cli
        .pr
        .clone()
        .or_else(|| pr_ctx.as_ref().map(|c| c.id.clone()));
    let token = forge.token();

    // Comment only with both a PR and a token; otherwise print for piping.
    if !cli.no_comment
        && let (Some(pr_id), Some(token)) = (pr_id, token)
    {
        let repo_ref = match &cli.repo {
            Some(slug) => {
                let (owner, name) = slug
                    .split_once('/')
                    .ok_or_else(|| anyhow::anyhow!("--repo must be in 'owner/name' form"))?;
                forge::RepoRef {
                    owner: owner.to_string(),
                    repo: name.to_string(),
                    host: "github.com".to_string(),
                }
            }
            None => forge.detect_repo(repo, &cfg.git.remote)?,
        };
        let api_url = forge.api_base_uri(&repo_ref);
        let action = forge.upsert_pr_comment(
            &token,
            api_url.as_deref(),
            &repo_ref,
            &pr_id,
            preview::PREVIEW_MARKER,
            &markdown,
        )?;
        eprintln!(
            "{} release preview comment on {}/{} #{}",
            action.verb(),
            repo_ref.owner,
            repo_ref.repo,
            pr_id
        );
    } else {
        println!("{}", markdown);
    }

    Ok(())
}

/// Retain only releases whose package matches the branch's package filter
/// (an empty filter keeps all). Returns how many releases were removed.
pub fn apply_branch_package_filter(
    releases: &mut Vec<version::PackageRelease>,
    branch_ctx: &config::BranchContext,
) -> usize {
    if branch_ctx.packages.is_empty() {
        return 0;
    }
    let before = releases.len();
    releases.retain(|r| {
        branch_ctx
            .packages
            .iter()
            .any(|pat| config::glob_match(pat, &r.package_name))
    });
    before - releases.len()
}

/// Whether a step should run on the current branch given its `branches` filter.
pub fn step_runs_on_branch(step_cfg: &config::StepConfig, branch_name: &str) -> bool {
    step_cfg.branches.is_empty()
        || step_cfg
            .branches
            .iter()
            .any(|pat| config::glob_match(pat, branch_name))
}

/// Narrow packages and releases to those a step operates on (its `packages`
/// glob filter). An empty filter means all.
pub fn filter_for_step(
    step_cfg: &config::StepConfig,
    packages: &[package::Package],
    releases: &[version::PackageRelease],
) -> (Vec<package::Package>, Vec<version::PackageRelease>) {
    if step_cfg.packages.is_empty() {
        return (packages.to_vec(), releases.to_vec());
    }
    let fp = packages
        .iter()
        .filter(|p| step_covers_package(step_cfg, &p.name))
        .cloned()
        .collect();
    let fr = releases
        .iter()
        .filter(|r| step_covers_package(step_cfg, &r.package_name))
        .cloned()
        .collect();
    (fp, fr)
}

/// Whether a step operates on a package, given its `packages` glob filter
/// (an empty filter matches all packages).
fn step_covers_package(step_cfg: &config::StepConfig, package_name: &str) -> bool {
    step_cfg.packages.is_empty()
        || step_cfg
            .packages
            .iter()
            .any(|pat| config::glob_match(pat, package_name))
}

/// Whether a `changelog` step would generate notes for this package on this
/// branch — mirrors the per-step branch + package filtering used at release time.
fn changelog_covers(cfg: &config::Config, branch_name: &str, package_name: &str) -> bool {
    cfg.steps.iter().any(|s| {
        s.name == "changelog"
            && step_runs_on_branch(s, branch_name)
            && step_covers_package(s, package_name)
    })
}

/// Run each step's `release` phase after the git commit and tags are pushed.
pub fn run_release_phase(
    repo_root: &std::path::Path,
    repo: &git2::Repository,
    cfg: &config::Config,
    branch_ctx: &config::BranchContext,
    packages: &[package::Package],
    releases: &[version::PackageRelease],
    dry_run: bool,
) -> Result<()> {
    let has_release_work = cfg
        .steps
        .iter()
        .filter(|s| step_runs_on_branch(s, &branch_ctx.branch_name))
        .filter(|s| step::create_step(&s.name).is_some_and(|p| p.has_release_phase()))
        .any(|s| {
            releases
                .iter()
                .any(|r| step_covers_package(s, &r.package_name))
        });
    if has_release_work {
        printfl!("{} Publishing releases", style(">>").bold().blue());
    }

    let release_ctx = step::ReleaseContext {
        repo_root,
        dry_run,
        branch: branch_ctx,
        cfg,
        repo,
    };

    for step_cfg in &cfg.steps {
        if !step_runs_on_branch(step_cfg, &branch_ctx.branch_name) {
            continue;
        }
        // Unknown step names were already warned about in the main step loop.
        let Some(p) = step::create_step(&step_cfg.name) else {
            continue;
        };
        let (filtered_packages, filtered_releases) = filter_for_step(step_cfg, packages, releases);
        p.release(
            &release_ctx,
            step_cfg,
            &filtered_packages,
            &filtered_releases,
        )?;
    }

    Ok(())
}
