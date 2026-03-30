mod commit;
mod config;
mod git;
mod package;
mod plugin;
mod version;

use anyhow::{Context, Result};
use clap::Parser;
use console::style;
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
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let repo_root = config::find_repo_root(&cli.path)
        .context("Could not find a git repository")?;

    let cfg = if let Some(config_path) = &cli.config {
        config::load_config(config_path)?
    } else {
        config::load_config(&repo_root)?
    };

    if cli.dry_run {
        println!(
            "{} {}",
            style("super-release").bold().cyan(),
            style("(dry run)").dim()
        );
    } else {
        println!("{}", style("super-release").bold().cyan());
    }
    println!();

    // 1. Discover packages
    let mut packages = package::discover_packages(&repo_root)?;
    package::resolve_local_dependencies(&mut packages);

    // Filter packages if specified
    if !cli.package.is_empty() {
        packages.retain(|p| cli.package.iter().any(|f| p.name.contains(f)));
    }

    // Apply exclude patterns from config
    if !cfg.exclude.is_empty() {
        packages.retain(|p| !cfg.exclude.iter().any(|e| p.name.contains(e)));
    }

    if packages.is_empty() {
        println!("{}", style("No packages found.").yellow());
        return Ok(());
    }

    println!(
        "{} Discovered {} package(s):",
        style(">>").bold().blue(),
        packages.len()
    );
    for pkg in &packages {
        println!(
            "   {} {} ({})",
            style("*").dim(),
            style(&pkg.name).bold(),
            style(pkg.path.display()).dim()
        );
    }
    println!();

    // 2. Open repo and determine releases
    let repo = git::open_repo(&repo_root)?;
    let releases = version::determine_releases(&repo, &packages)?;

    if releases.is_empty() {
        println!(
            "{} {}",
            style(">>").bold().blue(),
            style("No releases needed. All packages are up to date.").green()
        );
        return Ok(());
    }

    // 3. Print release plan
    println!(
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

        println!(
            "   {} {} {} -> {} ({})",
            style("*").dim(),
            style(&release.package_name).bold(),
            style(&release.current_version).dim(),
            style(&release.next_version).bold().fg(bump_color),
            style(&release.bump).fg(bump_color)
        );

        if cli.verbose || cli.dry_run {
            for commit in &release.commits {
                let type_str = if commit.breaking {
                    format!("{}!", commit.commit_type)
                } else {
                    commit.commit_type.clone()
                };
                println!(
                    "     {} {} {}",
                    style(&commit.hash).dim(),
                    style(format!("({})", type_str)).dim(),
                    commit.description
                );
            }
        }
    }
    println!();

    // 4. Execute plugins
    let plugin_ctx = plugin::PluginContext {
        repo_root: &repo_root,
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

        println!(
            "{} Running plugin: {}",
            style(">>").bold().blue(),
            style(p.name()).bold()
        );

        p.verify(&plugin_ctx, plugin_cfg)?;
        p.prepare(&plugin_ctx, plugin_cfg, &packages, &releases)?;
        p.publish(&plugin_ctx, plugin_cfg, &packages, &releases)?;

        println!();
    }

    if cli.dry_run {
        println!(
            "{}",
            style("Dry run complete. No changes were made.").green().bold()
        );
    } else {
        println!(
            "{}",
            style("Release complete!").green().bold()
        );
    }

    Ok(())
}
