#[macro_use]
mod macros;

mod cli;
mod commit;
mod config;
mod forge;
mod git;
mod notes;
mod package;
mod pm;
mod preview;
mod resolver;
mod run;
mod step;
mod version;

use anyhow::{Context, Result};
use clap::Parser;
use cli::Cli;
use console::style;

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

    // Must run before the HEAD branch gate: a PR branch is usually not a configured release branch, and preview must not be gated out.
    if cli.preview {
        return run::run_preview(&cli, &repo, &repo_root, &cfg, &packages);
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

    let skipped = run::apply_branch_package_filter(&mut releases, &branch_ctx);
    verbosefl!(verbose_mode, {
        if skipped > 0 {
            printfl!(
                "{} Skipped {} package(s) not included in branch '{}' config",
                style(">>").dim(),
                skipped,
                branch_ctx.branch_name
            );
        }
    });

    if cli.show_next_version {
        return run::show_next_version(&packages, &releases, cli.package.as_deref());
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
        if !run::step_runs_on_branch(step_cfg, &branch_ctx.branch_name) {
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
            run::filter_for_step(step_cfg, &packages, &releases);
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

    printfl!(
        "{} Finalizing git commit and tags",
        style(">>").bold().blue()
    );
    run::finalize_git(
        &repo_root,
        &repo,
        &cfg,
        &releases,
        &modified_files,
        cli.dry_run,
    )?;

    // Publishes to external services (e.g. GitHub Releases) only after the commit and tags exist on the remote.
    run::run_release_phase(
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
