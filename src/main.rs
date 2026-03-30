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

#[derive(Parser, Debug)]
#[command(
    name = "super-release",
    about = "A fast semantic-release alternative for monorepos",
    version,
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

    /// Only process specific packages (can be specified multiple times)
    #[arg(long, short = 'p')]
    package: Vec<String>,

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

    let (repo_root, repo) = config::find_repo_root(&cli.path)
        .context("Could not find a git repository")?;

    let cfg = if let Some(config_path) = &cli.config {
        config::load_config(config_path)?
    } else {
        config::load_config(&repo_root)?
    };

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

    let pkg_resolver = resolver::create_resolver("node")
        .expect("node resolver must exist");
    let mut packages = pkg_resolver.discover(&repo_root)?;
    pkg_resolver.resolve_dependencies(&mut packages);

    if !cli.package.is_empty() {
        packages.retain(|p| {
            cli.package.iter().any(|f| {
                p.name.contains(f) || config::glob_match(f, &p.name)
            })
        });
    }

    if let Some(ref include) = cfg.packages {
        packages.retain(|p| include.iter().any(|pat| config::glob_match(pat, &p.name)));
    }

    if !cfg.exclude.is_empty() {
        packages.retain(|p| !cfg.exclude.iter().any(|pat| config::glob_match(pat, &p.name)));
    }

    if packages.is_empty() {
        printfl!("{}", style("No packages found.").yellow());
        return Ok(());
    }

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
            style(pkg.path.display()).dim()
        );
    }
    printfl!();

    let branch_ctx = config::resolve_branch_context(&repo, &cfg)?;

    if cli.verbose || cli.dry_run {
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

    let num_threads = rayon::current_num_threads();
    printfl!(
        "{} Using {} thread{}",
        style(">>").bold().blue(),
        style(num_threads).bold(),
        if num_threads == 1 { "" } else { "s" }
    );
    printfl!();

    let releases =
        version::determine_releases(&repo, &repo_root, &packages, &cfg, &branch_ctx)?;

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

    let plugin_ctx = plugin::PluginContext {
        repo_root: &repo_root,
        repo: &repo,
        dry_run: cli.dry_run,
        config: &cfg,
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

        p.verify(&plugin_ctx, plugin_cfg)?;
        p.prepare(&plugin_ctx, plugin_cfg, &packages, &releases)?;
        p.publish(&plugin_ctx, plugin_cfg, &packages, &releases)?;

        printfl!();
    }

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
