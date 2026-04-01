mod commit;
mod config;
mod git;
mod package;
mod pm;
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

    let quiet = cli.show_next_version;

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
    let mut packages = pkg_resolver.discover(&repo_root)?;
    pkg_resolver.resolve_dependencies(&mut packages);
    package::sort_by_path_depth(&mut packages);

    if let Some(ref include) = cfg.packages {
        let before = packages.len();
        packages.retain(|p| include.iter().any(|pat| config::glob_match(pat, &p.name)));
        if !quiet && (cli.verbose || cli.dry_run) && packages.len() < before {
            let excluded = before - packages.len();
            printfl!(
                "{} Filtered {} package(s) by 'packages' include patterns",
                style(">>").dim(),
                excluded
            );
        }
    }

    if !cfg.exclude.is_empty() {
        let before = packages.len();
        packages.retain(|p| {
            !cfg.exclude
                .iter()
                .any(|pat| config::glob_match(pat, &p.name))
        });
        if !quiet && (cli.verbose || cli.dry_run) && packages.len() < before {
            let excluded = before - packages.len();
            printfl!(
                "{} Excluded {} package(s) by 'exclude' patterns",
                style(">>").dim(),
                excluded
            );
        }
    }

    if packages.is_empty() {
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
        for pkg in &packages {
            printfl!(
                "   {} {} ({})",
                style("*").dim(),
                style(&pkg.name).bold(),
                style(if pkg.path.as_os_str().is_empty() {
                    ".".into()
                } else {
                    pkg.path.display().to_string()
                })
                .dim()
            );
        }
        printfl!();
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

    if !quiet && (cli.verbose || cli.dry_run) {
        let channel_info = if let Some(ref pre) = branch_ctx.prerelease {
            format!(" (prerelease: {})", pre)
        } else if branch_ctx.maintenance {
            " (maintenance)".to_string()
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
    }

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
        let skipped = before - releases.len();
        if skipped > 0 && !quiet && (cli.verbose || cli.dry_run) {
            printfl!(
                "{} Skipped {} package(s) not included in branch '{}' config",
                style(">>").dim(),
                skipped,
                branch_ctx.branch_name
            );
        }
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

        printfl!(
            "   {} {} {} -> {} ({})",
            style("*").dim(),
            style(&release.package_name).bold(),
            style(&release.current_version).dim(),
            style(&release.next_version).bold().fg(bump_color),
            style(&release.bump).fg(bump_color)
        );

        if cli.verbose || cli.dry_run {
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
        }
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
    };

    for step_cfg in &cfg.steps {
        // Skip step if it's not configured for this branch
        if !step_cfg.branches.is_empty()
            && !step_cfg
                .branches
                .iter()
                .any(|pat| config::glob_match(pat, &branch_ctx.branch_name))
        {
            if cli.verbose || cli.dry_run {
                printfl!(
                    "{} Skipping step '{}' (not configured for branch '{}')",
                    style(">>").dim(),
                    step_cfg.name,
                    branch_ctx.branch_name
                );
            }
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

        let (filtered_packages, filtered_releases) = if step_cfg.packages.is_empty() {
            (packages.clone(), releases.clone())
        } else {
            let fp: Vec<_> = packages
                .iter()
                .filter(|p| {
                    step_cfg
                        .packages
                        .iter()
                        .any(|pat| config::glob_match(pat, &p.name))
                })
                .cloned()
                .collect();
            let fr: Vec<_> = releases
                .iter()
                .filter(|r| {
                    step_cfg
                        .packages
                        .iter()
                        .any(|pat| config::glob_match(pat, &r.package_name))
                })
                .cloned()
                .collect();
            if cli.verbose || cli.dry_run {
                let skipped = releases.len() - fr.len();
                if skipped > 0 {
                    printfl!(
                        "  {} Filtered to {} of {} release(s) by step packages filter",
                        style(">>").dim(),
                        fr.len(),
                        releases.len()
                    );
                }
            }
            (fp, fr)
        };

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
    if cfg.git.push && (!created_tags.is_empty() || has_staged) {
        printfl!("  [git] Pushing to {} ...", cfg.git.remote);
        let mut push_cmd = Command::new("git");
        push_cmd
            .arg("push")
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
