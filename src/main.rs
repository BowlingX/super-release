mod commit;
mod config;
mod git;
mod package;
mod plugin;
mod pm;
mod resolver;
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

    /// Number of parallel jobs for commit analysis (default: number of CPUs)
    #[arg(long, short = 'j')]
    jobs: Option<usize>,
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

    let cfg = if let Some(config_path) = &cli.config {
        config::load_config(config_path)?
    } else {
        config::load_config(&repo_root)?
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

    let jobs = cli.jobs.unwrap_or_else(|| {
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        (cpus / 2).max(1)
    });
    rayon::ThreadPoolBuilder::new()
        .num_threads(jobs)
        .build_global()
        .ok();

    if !quiet {
        let num_threads = rayon::current_num_threads();
        printfl!(
            "{} Using {} thread{}",
            style(">>").bold().blue(),
            style(num_threads).bold(),
            if num_threads == 1 { "" } else { "s" }
        );
        printfl!();
    }

    let releases = version::determine_releases(&repo, &repo_root, &packages, &cfg, &branch_ctx)?;

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

    // Core: bump package manifest versions before plugins run
    printfl!("{} Bumping package versions", style(">>").bold().blue());
    let mut modified_files =
        pkg_resolver.bump_versions(&repo_root, &packages, &releases, cli.dry_run)?;

    let plugin_ctx = plugin::PluginContext {
        repo_root: &repo_root,
        dry_run: cli.dry_run,
        branch: &branch_ctx,
    };

    for plugin_cfg in &cfg.plugins {
        let p = match plugin::create_plugin(&plugin_cfg.name) {
            Some(p) => p,
            None => {
                eprintln!(
                    "{} Unknown plugin: {}",
                    style("warning:").yellow().bold(),
                    plugin_cfg.name
                );
                continue;
            }
        };

        printfl!(
            "{} Running plugin: {}",
            style(">>").bold().blue(),
            style(p.name()).bold()
        );

        let (filtered_packages, filtered_releases) = if plugin_cfg.packages.is_empty() {
            (packages.clone(), releases.clone())
        } else {
            let fp: Vec<_> = packages
                .iter()
                .filter(|p| {
                    plugin_cfg
                        .packages
                        .iter()
                        .any(|pat| config::glob_match(pat, &p.name))
                })
                .cloned()
                .collect();
            let fr: Vec<_> = releases
                .iter()
                .filter(|r| {
                    plugin_cfg
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
                        "  {} Filtered to {} of {} release(s) by plugin packages filter",
                        style(">>").dim(),
                        fr.len(),
                        releases.len()
                    );
                }
            }
            (fp, fr)
        };

        p.verify(&plugin_ctx, plugin_cfg)?;
        modified_files.extend(p.prepare(
            &plugin_ctx,
            plugin_cfg,
            &filtered_packages,
            &filtered_releases,
        )?);
        modified_files.extend(p.publish(
            &plugin_ctx,
            plugin_cfg,
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

    // Stage only the files that plugins modified
    if !modified_files.is_empty() {
        let mut add_cmd = Command::new("git");
        add_cmd.arg("add").current_dir(repo_root);
        for f in modified_files {
            add_cmd.arg(f);
        }
        // Also stage any untracked changes from exec plugin (which can't report files)
        // by adding the whole repo — but only if exec was used
        let output = add_cmd.output().context("Failed to run git add")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("git add failed: {}", stderr);
        }
    }

    // Also stage any changes from exec plugin (it returns empty file lists)
    // Check if there are unstaged changes beyond what we tracked
    let has_unstaged = !Command::new("git")
        .args(["diff", "--quiet"])
        .current_dir(repo_root)
        .status()?
        .success();

    if has_unstaged {
        let output = Command::new("git")
            .args(["add", "-u"])
            .current_dir(repo_root)
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("git add -u failed: {}", stderr);
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
