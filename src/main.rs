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
        packages.retain(|p| include.iter().any(|pat| config::glob_match(pat, &p.name)));
    }

    if !cfg.exclude.is_empty() {
        packages.retain(|p| {
            !cfg.exclude
                .iter()
                .any(|pat| config::glob_match(pat, &p.name))
        });
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

    let plugin_ctx = plugin::PluginContext {
        repo_root: &repo_root,
        repo: &repo,
        dry_run: cli.dry_run,
        config: &cfg,
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
                        packages.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")
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
