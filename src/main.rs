mod commit;
mod config;
mod forge;
mod git;
mod package;
mod pm;
mod preview;
mod resolver;
mod step;
mod version;

use anyhow::{Context, Result};
use clap::Parser;
use console::style;
use std::io::{self, Write};
use std::path::PathBuf;

/// Version: prefer SUPER_RELEASE_VERSION env at runtime, fallback to Cargo.toml.
fn runtime_version() -> &'static str {
    match std::env::var("SUPER_RELEASE_VERSION") {
        Ok(v) => Box::leak(v.into_boxed_str()),
        Err(_) => env!("CARGO_PKG_VERSION"),
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "super-release",
    about = "A fast semantic-release alternative for monorepos",
    version = runtime_version(),
    author
)]
struct Cli {
    /// Run in dry-run mode (show what would happen without making changes)
    #[arg(long, short = 'n')]
    dry_run: bool,

    /// Path to the repository root (defaults to current directory)
    #[arg(long, short = 'C', default_value = ".")]
    path: PathBuf,

    /// Path to the config file (defaults to .release.yaml in repo root)
    #[arg(long, short = 'c')]
    config: Option<PathBuf>,

    /// Print the next version for a package and exit.
    /// Use --package to select which package (defaults to root).
    #[arg(long)]
    show_next_version: bool,

    /// Render a release preview (next versions + notes) for a pull request and
    /// exit. Posts/updates a sticky PR comment when a GitHub token and PR are
    /// detected; otherwise prints the Markdown to stdout. Makes no changes.
    #[arg(long)]
    preview: bool,

    /// Pull request number/id for --preview (defaults to the PR detected from
    /// the CI environment).
    #[arg(long)]
    pr: Option<String>,

    /// GitHub repository as `owner/name` for --preview (defaults to the git
    /// remote or the GITHUB_REPOSITORY environment variable).
    #[arg(long)]
    repo: Option<String>,

    /// Base branch to evaluate --preview against (defaults to the PR base
    /// branch, GITHUB_BASE_REF, or the first configured release branch).
    #[arg(long)]
    base: Option<String>,

    /// With --preview, never post a PR comment; always print Markdown to stdout.
    #[arg(long)]
    no_comment: bool,

    /// Filter to a specific package (used with --show-next-version)
    #[arg(long, short = 'p')]
    package: Option<String>,

    /// Verbose output
    #[arg(long, short = 'v')]
    verbose: bool,

    /// Skip config file validation against the JSON schema
    #[arg(long)]
    dangerously_skip_config_check: bool,
}

/// Print and immediately flush to ensure output is visible.
macro_rules! printfl {
    ($($arg:tt)*) => {{
        println!($($arg)*);
        let _ = io::stdout().flush();
    }};
}

/// Execute a block or print a line only in verbose mode.
///
/// Usage:
///   verbosefl!(flag, "format {}", arg);        // single print
///   verbosefl!(flag);                           // empty line
///   verbosefl!(flag, { ... });                  // arbitrary block
macro_rules! verbosefl {
    ($verbose:expr) => {{ if $verbose { printfl!(); } }};
    ($verbose:expr, { $($body:tt)* }) => {{
        if $verbose { $($body)* }
    }};
    ($verbose:expr, $($arg:tt)*) => {{
        if $verbose { printfl!($($arg)*); }
    }};
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let (repo_root, repo) =
        config::find_repo_root(&cli.path).context("Could not find a git repository")?;

    let config_source = &cli.config.clone().unwrap_or(repo_root.clone());

    let cfg = match config::schema::find_config(config_source)? {
        Some((content, file_path, format)) => {
            if !cli.dangerously_skip_config_check {
                let errors = config::schema::validate(&content, format);
                if !errors.is_empty() {
                    eprintln!(
                        "{} Invalid config file: {}",
                        style("error:").red().bold(),
                        file_path.display()
                    );
                    for err in &errors {
                        eprintln!("  {} {}", style("•").red(), err);
                    }
                    eprintln!(
                        "\n  Use {} to bypass this check.",
                        style("--dangerously-skip-config-check").yellow()
                    );
                    std::process::exit(1);
                }
            }
            config::schema::parse_config(&content, format)
                .with_context(|| format!("parsing config file: {}", file_path.display()))?
        }
        None => config::Config::default(),
    };

    let quiet = cli.show_next_version || cli.preview;
    let verbose_mode = !quiet && (cli.verbose || cli.dry_run);

    if !quiet {
        if cli.dry_run {
            printfl!(
                "{} {}",
                style("super-release").bold().cyan(),
                style("(dry run)").dim()
            );
        } else {
            printfl!("{}", style("super-release").bold().cyan());
        }
        printfl!();
    }

    let pkg_resolver = resolver::create_resolver("node").expect("node resolver must exist");
    let discovered = pkg_resolver.discover(&repo_root)?;

    // Separate skipped packages (e.g. missing name) from valid ones
    let (skipped, mut packages): (Vec<_>, Vec<_>) = discovered.into_iter().partition(|p| p.skipped);
    pkg_resolver.resolve_dependencies(&mut packages);
    package::sort_by_path_depth(&mut packages);

    if let Some(ref include) = cfg.packages {
        let before = packages.len();
        packages.retain(|p| include.iter().any(|pat| config::glob_match(pat, &p.name)));
        verbosefl!(verbose_mode, {
            if packages.len() < before {
                printfl!(
                    "{} Filtered {} package(s) by 'packages' include patterns",
                    style(">>").dim(),
                    before - packages.len()
                );
            }
        });
    }

    if !cfg.exclude.is_empty() {
        let before = packages.len();
        packages.retain(|p| {
            !cfg.exclude
                .iter()
                .any(|pat| config::glob_match(pat, &p.name))
        });
        verbosefl!(verbose_mode, {
            if packages.len() < before {
                printfl!(
                    "{} Excluded {} package(s) by 'exclude' patterns",
                    style(">>").dim(),
                    before - packages.len()
                );
            }
        });
    }

    if packages.is_empty() && skipped.is_empty() {
        if !quiet {
            printfl!("{}", style("No packages found.").yellow());
        }
        return Ok(());
    }

    if !quiet {
        printfl!(
            "{} Discovered {} package(s):",
            style(">>").bold().blue(),
            packages.len()
        );
        for pkg in packages.iter().chain(&skipped) {
            let path_display = if pkg.path.as_os_str().is_empty() {
                ".".to_string()
            } else {
                pkg.path.display().to_string()
            };
            let display_name = if pkg.skipped {
                pkg.manifest_path.display().to_string()
            } else {
                pkg.name.clone()
            };
            if let Some(ref warning) = pkg.warning {
                printfl!(
                    "   {} {} ({}) — {}",
                    style("*").yellow(),
                    style(&display_name).bold(),
                    style(&path_display).dim(),
                    style(warning).yellow()
                );
            } else {
                printfl!(
                    "   {} {} ({})",
                    style("*").dim(),
                    style(&display_name).bold(),
                    style(&path_display).dim()
                );
            }
        }
        printfl!();
    }

    if packages.is_empty() {
        if !quiet {
            printfl!("{}", style("No packages found.").yellow());
        }
        return Ok(());
    }

    // Runs before the HEAD branch gate below: a PR branch is usually not itself
    // a configured release branch, and preview must not be gated out.
    if cli.preview {
        return run_preview(&cli, &repo, &repo_root, &cfg, &packages);
    }

    let branch_ctx = match config::resolve_branch_context(&repo, &cfg)? {
        Some(ctx) => ctx,
        None => {
            let head = repo
                .head()
                .ok()
                .and_then(|h| h.shorthand().map(String::from));
            let branch = head.as_deref().unwrap_or("HEAD");
            printfl!(
                "{} Branch '{}' is not configured for releases, skipping.",
                style(">>").bold().yellow(),
                style(branch).bold()
            );
            return Ok(());
        }
    };

    if !cli.dry_run && !quiet {
        git::check_branch_up_to_date(&repo_root, &repo, &branch_ctx.branch_name)?;
    }

    verbosefl!(verbose_mode, {
        let channel_info = if let Some(ref pre) = branch_ctx.prerelease {
            format!(" (prerelease: {})", pre)
        } else if branch_ctx.maintenance {
            " (maintenance)".to_string()
        } else if let Some(ref ch) = branch_ctx.channel {
            format!(" (channel: {})", ch)
        } else {
            String::new()
        };
        printfl!(
            "{} Branch: {}{}",
            style(">>").bold().blue(),
            style(&branch_ctx.branch_name).bold(),
            style(channel_info).dim()
        );
        printfl!();
    });

    let mut releases =
        version::determine_releases(&repo, &repo_root, &packages, &cfg, &branch_ctx)?;

    // Apply branch-level package filter
    if !branch_ctx.packages.is_empty() {
        let before = releases.len();
        releases.retain(|r| {
            branch_ctx
                .packages
                .iter()
                .any(|pat| config::glob_match(pat, &r.package_name))
        });
        verbosefl!(verbose_mode, {
            let skipped = before - releases.len();
            if skipped > 0 {
                printfl!(
                    "{} Skipped {} package(s) not included in branch '{}' config",
                    style(">>").dim(),
                    skipped,
                    branch_ctx.branch_name
                );
            }
        });
    }

    if cli.show_next_version {
        return show_next_version(&packages, &releases, cli.package.as_deref());
    }

    if releases.is_empty() {
        printfl!(
            "{} {}",
            style(">>").bold().blue(),
            style("No releases needed. All packages are up to date.").green()
        );
        return Ok(());
    }

    printfl!(
        "{} Release plan ({} package(s) to release):\n",
        style(">>").bold().blue(),
        releases.len()
    );

    for release in &releases {
        let bump_color = match release.bump {
            commit::BumpLevel::Major => console::Color::Red,
            commit::BumpLevel::Minor => console::Color::Yellow,
            commit::BumpLevel::Patch => console::Color::Green,
            commit::BumpLevel::None => console::Color::White,
        };

        if let Some(ref reason) = release.propagated_from {
            printfl!(
                "   {} {} {} -> {} ({}, dependency updated: {})",
                style("*").dim(),
                style(&release.package_name).bold(),
                style(&release.current_version).dim(),
                style(&release.next_version).bold().fg(bump_color),
                style(&release.bump).fg(bump_color),
                style(reason).cyan()
            );
        } else {
            printfl!(
                "   {} {} {} -> {} ({})",
                style("*").dim(),
                style(&release.package_name).bold(),
                style(&release.current_version).dim(),
                style(&release.next_version).bold().fg(bump_color),
                style(&release.bump).fg(bump_color)
            );
        }

        verbosefl!(verbose_mode, {
            const MAX_COMMITS_SHOWN: usize = 10;
            for commit in release.commits.iter().take(MAX_COMMITS_SHOWN) {
                let type_str = if commit.breaking {
                    format!("{}!", commit.commit_type)
                } else {
                    commit.commit_type.clone()
                };
                printfl!(
                    "     {} {} {}",
                    style(&commit.hash).dim(),
                    style(format!("({})", type_str)).dim(),
                    commit.description
                );
            }
            if release.commits.len() > MAX_COMMITS_SHOWN {
                printfl!(
                    "     {} (+{} more commits)",
                    style("...").dim(),
                    release.commits.len() - MAX_COMMITS_SHOWN
                );
            }
        });
    }
    printfl!();

    // Core: bump package manifest versions before steps run
    printfl!("{} Bumping package versions", style(">>").bold().blue());
    let mut modified_files =
        pkg_resolver.bump_versions(&repo_root, &packages, &releases, cli.dry_run)?;

    let step_ctx = step::StepContext {
        repo_root: &repo_root,
        dry_run: cli.dry_run,
        branch: &branch_ctx,
        cfg: &cfg,
    };

    for step_cfg in &cfg.steps {
        if !step_runs_on_branch(step_cfg, &branch_ctx.branch_name) {
            verbosefl!(
                verbose_mode,
                "{} Skipping step '{}' (not configured for branch '{}')",
                style(">>").dim(),
                step_cfg.name,
                branch_ctx.branch_name
            );
            continue;
        }

        let p = match step::create_step(&step_cfg.name) {
            Some(p) => p,
            None => {
                eprintln!(
                    "{} Unknown step: {}",
                    style("warning:").yellow().bold(),
                    step_cfg.name
                );
                continue;
            }
        };

        printfl!(
            "{} Running step: {}",
            style(">>").bold().blue(),
            style(p.name()).bold()
        );

        let (filtered_packages, filtered_releases) =
            filter_for_step(step_cfg, &packages, &releases);
        verbosefl!(verbose_mode, {
            let skipped = releases.len() - filtered_releases.len();
            if skipped > 0 {
                printfl!(
                    "  {} Filtered to {} of {} release(s) by step packages filter",
                    style(">>").dim(),
                    filtered_releases.len(),
                    releases.len()
                );
            }
        });

        p.verify(&step_ctx, step_cfg)?;
        modified_files.extend(p.prepare(
            &step_ctx,
            step_cfg,
            &filtered_packages,
            &filtered_releases,
        )?);
        modified_files.extend(p.publish(
            &step_ctx,
            step_cfg,
            &filtered_packages,
            &filtered_releases,
        )?);

        printfl!();
    }

    // Core: git commit + tag
    printfl!(
        "{} Finalizing git commit and tags",
        style(">>").bold().blue()
    );
    finalize_git(
        &repo_root,
        &repo,
        &cfg,
        &releases,
        &modified_files,
        cli.dry_run,
    )?;

    // Release phase: publish to external services (e.g. GitHub Releases) now
    // that the commit and tags exist on the remote.
    run_release_phase(
        &repo_root,
        &repo,
        &cfg,
        &branch_ctx,
        &packages,
        &releases,
        cli.dry_run,
    )?;

    if cli.dry_run {
        printfl!(
            "{}",
            style("Dry run complete. No changes were made.")
                .green()
                .bold()
        );
    } else {
        printfl!("{}", style("Release complete!").green().bold());
    }

    Ok(())
}

/// Core git finalize: stage modified files, commit, tag, optionally push.
fn finalize_git(
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

    // Stage only the files that steps reported as modified
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

    // Check if there's anything to commit
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

    // Create tags
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

    // Push
    if cfg.git.push {
        // A concurrent release may already have pushed some of these tags
        // (e.g. the local checkout didn't fetch them); the remote rejects
        // re-pushing an existing tag, which would fail the entire push.
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
            // All refs or nothing: a lost push race must not leave tags on
            // the remote whose release commit never landed on the branch.
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
fn show_next_version(
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

/// Render a release preview for a pull request and either post it as a sticky
/// PR comment or print the Markdown to stdout. Makes no changes to the repo.
fn run_preview(
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

    // Fall back to a plain stable context when the base isn't a configured
    // release branch, so the preview still reflects the commit bumps.
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
    if !branch_ctx.packages.is_empty() {
        releases.retain(|r| {
            branch_ctx
                .packages
                .iter()
                .any(|pat| config::glob_match(pat, &r.package_name))
        });
    }

    let markdown = preview::render_preview_markdown(&releases, cfg);

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

/// Whether a step should run on the current branch given its `branches` filter.
fn step_runs_on_branch(step_cfg: &config::StepConfig, branch_name: &str) -> bool {
    step_cfg.branches.is_empty()
        || step_cfg
            .branches
            .iter()
            .any(|pat| config::glob_match(pat, branch_name))
}

/// Narrow packages and releases to those a step operates on (its `packages`
/// glob filter). An empty filter means all.
fn filter_for_step(
    step_cfg: &config::StepConfig,
    packages: &[package::Package],
    releases: &[version::PackageRelease],
) -> (Vec<package::Package>, Vec<version::PackageRelease>) {
    if step_cfg.packages.is_empty() {
        return (packages.to_vec(), releases.to_vec());
    }
    let matches = |name: &str| {
        step_cfg
            .packages
            .iter()
            .any(|pat| config::glob_match(pat, name))
    };
    let fp = packages
        .iter()
        .filter(|p| matches(&p.name))
        .cloned()
        .collect();
    let fr = releases
        .iter()
        .filter(|r| matches(&r.package_name))
        .cloned()
        .collect();
    (fp, fr)
}

/// Run each step's `release` phase after the git commit and tags are pushed.
fn run_release_phase(
    repo_root: &std::path::Path,
    repo: &git2::Repository,
    cfg: &config::Config,
    branch_ctx: &config::BranchContext,
    packages: &[package::Package],
    releases: &[version::PackageRelease],
    dry_run: bool,
) -> Result<()> {
    if cfg.steps.iter().any(|s| s.name == "github") {
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
