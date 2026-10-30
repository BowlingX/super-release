//! Release orchestration: the phases `main` drives and the branch/package filtering shared across them.

use anyhow::{Context, Result};
use console::style;

use crate::cli::Cli;
use crate::forge::Forge;
use crate::{config, forge, git, output, package, pr_notes, preview, step, version};

/// Core git finalize: stage modified files, commit, tag, optionally push.
#[expect(
    clippy::too_many_lines,
    reason = "predates the limit; split when next touched"
)]
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

/// GitHub attribution and PR-comment notes for `releases`. `head` is the released
/// commit (first-time contributors, the merged release PR); `current_pr` the
/// previewed one. Best-effort: a failure warns and the notes fall back to commits.
fn release_metadata(
    token: &str,
    api_url: Option<&str>,
    gh_repo: &forge::RepoRef,
    cfg: &config::Config,
    releases: &[version::PackageRelease],
    head: Option<&str>,
    current_pr: Option<i64>,
) -> Option<pr_notes::ReleaseMetadata> {
    let forge = forge::github::GitHubForge;
    let mut shas: Vec<String> = releases
        .iter()
        .flat_map(|r| &r.commits)
        .filter_map(|c| c.full_sha())
        .collect();
    shas.sort();
    shas.dedup();
    let attribution = forge
        .release_attribution(token, api_url, gh_repo, head, &shas)
        .inspect_err(|e| {
            eprintln!(
                "  [github] Warning: could not fetch GitHub metadata, release notes credit no contributors or PRs and ignore PR comments: {:#}",
                e
            );
        })
        .ok()?;
    let pull_requests = if cfg.release_notes.pr_comments {
        forge
            .release_pull_requests(token, api_url, gh_repo, &attribution, head, current_pr)
            .unwrap_or_else(|e| {
                eprintln!(
                    "  [github] Warning: could not read PR comments, release notes ignore `@super-release release-notes`: {:#}",
                    e
                );
                Vec::new()
            })
    } else {
        Vec::new()
    };
    Some(pr_notes::ReleaseMetadata::new(
        attribution,
        pull_requests,
        cfg.release_notes.trusted_authors.clone(),
        releases,
    ))
}

/// The `github` step's `github_url`, else the remote's API base. Ignores the
/// step's branch filter: a preview needs it before it knows the base branch.
fn github_api_url(
    forge: &dyn forge::Forge,
    cfg: &config::Config,
    repo_ref: &forge::RepoRef,
) -> Option<String> {
    cfg.steps
        .iter()
        .find(|s| s.name == "github")
        .and_then(|s| step::parse_options::<step::github::GithubOptions>(s).ok())
        .and_then(|opts| opts.github_url)
        .or_else(|| forge.api_base_uri(repo_ref))
}

/// The run's GitHub metadata, looked up once before any step runs, for the
/// packages whose notes a step renders. `None` without a token or such a package.
pub fn fetch_release_metadata(
    repo: &git2::Repository,
    cfg: &config::Config,
    branch_ctx: &config::BranchContext,
    releases: &[version::PackageRelease],
    dry_run: bool,
) -> Option<pr_notes::ReleaseMetadata> {
    // The github step renders nothing in a dry run or without a push.
    let github_publishes = !dry_run && cfg.git.push;
    let noted = noted_releases(cfg, &branch_ctx.branch_name, releases, github_publishes);
    if noted.is_empty() {
        return None;
    }
    let forge = forge::github::GitHubForge;
    let token = forge.token()?;
    let gh_repo = forge
        .detect_repo(repo, &cfg.git.remote)
        .inspect_err(|e| eprintln!("  [github] Warning: {:#}", e))
        .ok()?;
    let api_url = github_api_url(&forge, cfg, &gh_repo);
    let meta = release_metadata(
        &token,
        api_url.as_deref(),
        &gh_repo,
        cfg,
        &noted,
        git::head_sha(repo).as_deref(),
        None,
    )?;

    let mut from_prs: Vec<i64> = noted
        .iter()
        .filter_map(|r| meta.notes_for(&r.package_name))
        .flat_map(|n| &n.blocks)
        .map(|b| b.pr_number)
        .collect();
    from_prs.sort_unstable();
    from_prs.dedup();
    if !from_prs.is_empty() {
        let list: Vec<String> = from_prs.iter().map(|n| format!("#{n}")).collect();
        printfl!(
            "{} Release notes from PR comments on {}",
            style(">>").dim(),
            list.join(", ")
        );
        printfl!();
    }
    Some(meta)
}

/// Where the preview reads the PR from and comments to.
struct PreviewRemote {
    pr_id: String,
    token: String,
    repo_ref: forge::RepoRef,
    api_url: Option<String>,
}

fn preview_repo_ref(
    cli: &Cli,
    forge: &dyn forge::Forge,
    repo: &git2::Repository,
    cfg: &config::Config,
) -> Result<forge::RepoRef> {
    match &cli.repo {
        Some(slug) => {
            let (owner, name) = slug
                .split_once('/')
                .ok_or_else(|| anyhow::anyhow!("--repo must be in 'owner/name' form"))?;
            Ok(forge::RepoRef {
                owner: owner.to_string(),
                repo: name.to_string(),
                host: "github.com".to_string(),
            })
        }
        None => forge.detect_repo(repo, &cfg.git.remote),
    }
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
    let pr_id = cli
        .pr
        .clone()
        .or_else(|| pr_ctx.as_ref().map(|c| c.id.clone()));

    // Reading and commenting on the PR need a PR and a token.
    let remote = match (pr_id, forge.token()) {
        (Some(pr_id), Some(token)) => match preview_repo_ref(cli, forge.as_ref(), repo, cfg) {
            Ok(repo_ref) => Some(PreviewRemote {
                api_url: github_api_url(forge.as_ref(), cfg, &repo_ref),
                pr_id,
                token,
                repo_ref,
            }),
            Err(e) if cli.no_comment => {
                eprintln!("Warning: not reading the pull request: {:#}", e);
                None
            }
            Err(e) => return Err(e),
        },
        _ => None,
    };

    let base_branch =
        preview_base_branch(cli, forge.as_ref(), pr_ctx.as_ref(), remote.as_ref(), cfg);

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

    // Previews check out PR heads, so HEAD is not the release branch's tip.
    let mut releases =
        version::determine_releases(repo, repo_root, packages, cfg, &branch_ctx, false)?;
    apply_branch_package_filter(&mut releases, &branch_ctx);

    // Only preview notes for packages a `changelog` step would cover on this branch, mirroring the per-step branch + package filtering.
    let notes_packages: std::collections::HashSet<String> = releases
        .iter()
        .filter(|r| step_covers(cfg, "changelog", &base_branch, &r.package_name))
        .map(|r| r.package_name.clone())
        .collect();
    let noted = noted_releases(cfg, &base_branch, &releases, true);

    let changelog_template = preview_changelog_template(repo_root, cfg, &base_branch);

    let meta = remote
        .as_ref()
        .filter(|_| cfg.release_notes.pr_comments)
        .and_then(|r| {
            release_metadata(
                &r.token,
                r.api_url.as_deref(),
                &r.repo_ref,
                cfg,
                &noted,
                // The PR head isn't merged yet, and previews credit no contributors.
                None,
                r.pr_id.parse().ok(),
            )
        });

    let markdown = preview::render_preview_markdown(
        &releases,
        &notes_packages,
        changelog_template.as_deref(),
        cfg,
        meta.as_ref(),
    );

    // Comment only with both a PR and a token; otherwise print for piping.
    let Some(r) = remote.filter(|_| !cli.no_comment) else {
        println!("{}", markdown);
        return Ok(());
    };
    let action = forge.upsert_pr_comment(
        &r.token,
        r.api_url.as_deref(),
        &r.repo_ref,
        &r.pr_id,
        preview::PREVIEW_MARKER,
        &markdown,
    )?;
    eprintln!(
        "{} release preview comment on {}/{} #{}",
        action.verb(),
        r.repo_ref.owner,
        r.repo_ref.repo,
        r.pr_id
    );

    if let (Some(comment_id), Some(meta)) =
        (pr_ctx.as_ref().and_then(|c| c.comment_id), meta.as_ref())
    {
        react_to_directive(forge.as_ref(), &r, comment_id, meta);
    }

    Ok(())
}

/// The PR's base branch: `--base`, the event, `GITHUB_BASE_REF`, the API (comment
/// events don't carry it), then the first configured branch.
fn preview_base_branch(
    cli: &Cli,
    forge: &dyn forge::Forge,
    pr_ctx: Option<&forge::PrContext>,
    remote: Option<&PreviewRemote>,
    cfg: &config::Config,
) -> String {
    cli.base
        .clone()
        .or_else(|| pr_ctx.and_then(|c| c.base_ref.clone()))
        .or_else(|| {
            std::env::var("GITHUB_BASE_REF")
                .ok()
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            let r = remote?;
            forge
                .pull_request_base_ref(&r.token, r.api_url.as_deref(), &r.repo_ref, &r.pr_id)
                .inspect_err(|e| {
                    eprintln!("Warning: could not look up the PR base branch: {:#}", e);
                })
                .ok()
        })
        .or_else(|| cfg.branches.first().map(|b| b.name().to_string()))
        .unwrap_or_else(|| "main".to_string())
}

/// The changelog step's template on `base_branch`. Best-effort: a template the
/// preview can't use falls back to the default.
fn preview_changelog_template(
    repo_root: &std::path::Path,
    cfg: &config::Config,
    base_branch: &str,
) -> Option<String> {
    let step = cfg
        .steps
        .iter()
        .find(|s| s.name == "changelog" && step_runs_on_branch(s, base_branch))?;
    let opts = step::parse_options::<step::changelog::ChangelogOptions>(step).ok()?;
    step::resolve_preview_template(
        repo_root,
        opts.template.as_deref(),
        opts.template_file.as_deref(),
    )
    .inspect_err(|e| {
        eprintln!(
            "Warning: previewing with the default changelog template: {:#}",
            e
        );
    })
    .ok()
    .flatten()
}

/// React to the directive comment that triggered the preview: 👀 when its notes
/// apply, 😕 when it's ignored. Anything else (or a deleted comment) is left alone.
fn react_to_directive(
    forge: &dyn forge::Forge,
    r: &PreviewRemote,
    comment_id: u64,
    meta: &pr_notes::ReleaseMetadata,
) {
    let Some(found) = pr_notes::directive_comments(meta)
        .into_iter()
        .find(|d| d.comment.id == comment_id)
    else {
        return;
    };
    let reaction = if found.ignored.is_some() {
        forge::CommentReaction::Confused
    } else {
        forge::CommentReaction::Seen
    };
    if let Err(e) = forge.react_to_comment(
        &r.token,
        r.api_url.as_deref(),
        &r.repo_ref,
        comment_id,
        reaction,
    ) {
        eprintln!(
            "Warning: could not react to comment {}: {:#}",
            comment_id, e
        );
    }
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
    releases.retain(|r| branch_ctx.includes_package(&r.package_name));
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

/// The releases whose notes a step renders from the GitHub metadata: the
/// `changelog` step with PR comments on, and the `github` step if `github`.
fn noted_releases(
    cfg: &config::Config,
    branch_name: &str,
    releases: &[version::PackageRelease],
    github: bool,
) -> Vec<version::PackageRelease> {
    releases
        .iter()
        .filter(|r| {
            let covers = |step| step_covers(cfg, step, branch_name, &r.package_name);
            (github && covers("github")) || (cfg.release_notes.pr_comments && covers("changelog"))
        })
        .cloned()
        .collect()
}

/// Whether a `step_name` step runs for this package on this branch — mirrors the
/// per-step branch + package filtering used at release time.
fn step_covers(
    cfg: &config::Config,
    step_name: &str,
    branch_name: &str,
    package_name: &str,
) -> bool {
    cfg.steps.iter().any(|s| {
        s.name == step_name
            && step_runs_on_branch(s, branch_name)
            && step_covers_package(s, package_name)
    })
}

/// Run each step's `release` phase after the git commit and tags are pushed.
pub fn run_release_phase(
    release_ctx: &step::ReleaseContext,
    packages: &[package::Package],
    releases: &[version::PackageRelease],
) -> Result<()> {
    let cfg = release_ctx.cfg;
    let branch_name = &release_ctx.branch.branch_name;
    let has_release_work = cfg
        .steps
        .iter()
        .filter(|s| step_runs_on_branch(s, branch_name))
        .filter(|s| step::create_step(&s.name).is_some_and(|p| p.has_release_phase()))
        .any(|s| {
            releases
                .iter()
                .any(|r| step_covers_package(s, &r.package_name))
        });
    let _section = has_release_work.then(|| output::section("Publishing releases"));

    for step_cfg in &cfg.steps {
        if !step_runs_on_branch(step_cfg, branch_name) {
            continue;
        }
        // Unknown step names were already warned about in the main step loop.
        let Some(p) = step::create_step(&step_cfg.name) else {
            continue;
        };
        let (filtered_packages, filtered_releases) = filter_for_step(step_cfg, packages, releases);
        p.release(
            release_ctx,
            step_cfg,
            &filtered_packages,
            &filtered_releases,
        )?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commit::BumpLevel;

    fn release(name: &str) -> version::PackageRelease {
        version::PackageRelease {
            package_name: name.into(),
            current_version: semver::Version::new(1, 0, 0),
            next_version: semver::Version::new(1, 1, 0),
            bump: BumpLevel::Minor,
            commits: vec![],
            is_root: false,
            propagated_from: None,
        }
    }

    fn names(releases: &[version::PackageRelease]) -> Vec<&str> {
        releases.iter().map(|r| r.package_name.as_str()).collect()
    }

    /// Only packages a `changelog` or `github` step covers get PR notes.
    #[test]
    fn noted_releases_follow_the_notes_steps() {
        let cfg: config::Config = serde_saphyr::from_str(
            "branches: [main]\n\
             steps:\n\
             - name: changelog\n  packages: [root]\n\
             - name: github\n  packages: [cli]\n\
             - name: npm\n  packages: [cli, cli-linux-x64]\n",
        )
        .unwrap();
        let releases = [release("root"), release("cli"), release("cli-linux-x64")];

        assert_eq!(
            names(&noted_releases(&cfg, "main", &releases, true)),
            ["root", "cli"]
        );
        // Without a published GitHub release, only the changelog renders notes.
        assert_eq!(
            names(&noted_releases(&cfg, "main", &releases, false)),
            ["root"]
        );
    }
}
