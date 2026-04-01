use anyhow::Result;
use rayon::prelude::*;
use serde::Deserialize;
use std::collections::HashMap;

use super::{Plugin, PluginConfig, PluginContext, parse_options, subprocess};
use crate::package::{Package, topological_sort};
use crate::pm::PackageManager;
use crate::version::PackageRelease;

/// Options for the npm/publish plugin.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct NpmOptions {
    /// Access level for publish ("public" or "restricted"). If unset, npm's default applies.
    #[serde(default)]
    pub access: Option<String>,

    /// Registry URL (overrides default)
    #[serde(default)]
    pub registry: Option<String>,

    /// Additional args passed to the publish command
    #[serde(default)]
    pub publish_args: Vec<String>,

    /// Dist-tag override. By default derived from the prerelease channel.
    #[serde(default)]
    pub tag: Option<String>,

    /// Enable npm provenance (--provenance). Requires npm 9.5+ and a supported CI.
    #[serde(default)]
    pub provenance: bool,

    /// Force a specific package manager (overrides auto-detection).
    #[serde(default)]
    pub package_manager: Option<PackageManager>,
}

pub struct NpmPlugin;

impl Plugin for NpmPlugin {
    fn name(&self) -> &str {
        "npm"
    }

    fn verify(&self, ctx: &PluginContext, config: &PluginConfig) -> Result<()> {
        if ctx.dry_run {
            return Ok(());
        }
        let opts: NpmOptions = parse_options(config)?;
        resolve_pm(ctx, &opts)?.verify()
    }

    fn publish(
        &self,
        ctx: &PluginContext,
        config: &PluginConfig,
        packages: &[Package],
        releases: &[PackageRelease],
    ) -> Result<Vec<std::path::PathBuf>> {
        let opts: NpmOptions = parse_options(config)?;
        let pm = resolve_pm(ctx, &opts)?;
        let dist_tag: Option<&str> = opts.tag.as_deref().or(ctx.branch.prerelease.as_deref());

        let order = topological_sort(packages)?;
        let release_set: HashMap<&str, &PackageRelease> = releases
            .iter()
            .map(|r| (r.package_name.as_str(), r))
            .collect();

        let levels = dependency_levels(&order, packages, &release_set);
        let repo_root = ctx.repo_root;
        let dry_run = ctx.dry_run;
        let mut errors: Vec<String> = Vec::new();

        for level in &levels {
            let results: Vec<Result<()>> = level
                .par_iter()
                .filter_map(|name| {
                    let release = release_set.get(name.as_str())?;
                    let pkg = packages.iter().find(|p| p.name == *name)?;
                    Some(publish_one(
                        repo_root, dry_run, pkg, release, &opts, pm, dist_tag,
                    ))
                })
                .collect();

            for r in results {
                if let Err(e) = r {
                    eprintln!("  [{}] Error: {}", pm, e);
                    errors.push(e.to_string());
                }
            }
        }

        if errors.is_empty() {
            Ok(Vec::new())
        } else {
            anyhow::bail!(
                "{} package(s) failed to publish:\n  {}",
                errors.len(),
                errors.join("\n  ")
            )
        }
    }
}

fn resolve_pm(ctx: &PluginContext, opts: &NpmOptions) -> Result<PackageManager> {
    match opts.package_manager {
        Some(pm) => Ok(pm),
        None => PackageManager::detect(ctx.repo_root),
    }
}

fn publish_one(
    repo_root: &std::path::Path,
    dry_run: bool,
    pkg: &Package,
    release: &PackageRelease,
    opts: &NpmOptions,
    pm: PackageManager,
    dist_tag: Option<&str>,
) -> Result<()> {
    let pkg_dir = repo_root.join(&pkg.path);
    let label = format!("{} v{}", pkg.name, release.next_version);
    let pm_name = pm.to_string();

    // Check if this exact version is already published
    if is_version_published(
        &pkg.name,
        &release.next_version.to_string(),
        opts.registry.as_deref(),
    ) {
        println!("  [{}] {} already published, skipping", pm_name, label);
        return Ok(());
    }

    let cmd = pm.publish_command(
        &pkg_dir,
        opts.access.as_deref(),
        opts.registry.as_deref(),
        dist_tag,
        opts.provenance,
        &opts.publish_args,
    );

    if dry_run {
        println!(
            "  [{}] Would publish {}: {}",
            pm_name,
            label,
            subprocess::format_command(&cmd)
        );
        println!(
            "    {}",
            console::style(format!("in {}", pkg_dir.display())).dim()
        );
        return Ok(());
    }

    println!(
        "  [{}] Publishing {}: {}",
        pm_name,
        label,
        subprocess::format_command(&cmd)
    );
    println!(
        "    {}",
        console::style(format!("in {}", pkg_dir.display())).dim()
    );
    subprocess::run_command(
        cmd,
        &subprocess::RunOptions {
            label: &label,
            plugin_name: &pm_name,
        },
    )
}

/// Check if a specific version of a package is already published to the registry.
/// Uses `npm view` which works regardless of the workspace package manager.
fn is_version_published(name: &str, version: &str, registry: Option<&str>) -> bool {
    use std::process::Command;

    let mut cmd = Command::new("npm");
    cmd.args(["view", &format!("{}@{}", name, version), "version"]);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::null());

    if let Some(reg) = registry {
        cmd.args(["--registry", reg]);
    }

    let Ok(output) = cmd.output() else {
        return false;
    };

    if !output.status.success() {
        return false;
    }

    String::from_utf8_lossy(&output.stdout).trim() == version
}

fn dependency_levels(
    topo_order: &[String],
    packages: &[Package],
    release_set: &HashMap<&str, &PackageRelease>,
) -> Vec<Vec<String>> {
    let mut levels: Vec<Vec<String>> = Vec::new();
    let mut pkg_level: HashMap<&str, usize> = HashMap::new();

    for name in topo_order {
        if !release_set.contains_key(name.as_str()) {
            continue;
        }

        let pkg = packages.iter().find(|p| p.name == *name);
        let level = pkg
            .map(|p| {
                p.local_dependencies
                    .keys()
                    .filter_map(|dep| pkg_level.get(dep.as_str()))
                    .max()
                    .map(|l| l + 1)
                    .unwrap_or(0)
            })
            .unwrap_or(0);

        pkg_level.insert(name, level);
        while levels.len() <= level {
            levels.push(Vec::new());
        }
        levels[level].push(name.clone());
    }

    levels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dependency_levels() {
        let packages = vec![
            make_pkg("a", &[]),
            make_pkg("b", &["a"]),
            make_pkg("c", &["a"]),
            make_pkg("d", &["b", "c"]),
        ];

        let releases: Vec<PackageRelease> = ["a", "b", "c", "d"]
            .iter()
            .map(|n| PackageRelease {
                package_name: n.to_string(),
                current_version: semver::Version::new(1, 0, 0),
                next_version: semver::Version::new(1, 1, 0),
                bump: crate::commit::BumpLevel::Minor,
                commits: vec![],
                is_root: false,
            })
            .collect();

        let release_set: HashMap<&str, &PackageRelease> = releases
            .iter()
            .map(|r| (r.package_name.as_str(), r))
            .collect();

        let order = topological_sort(&packages).unwrap();
        let levels = dependency_levels(&order, &packages, &release_set);

        assert_eq!(levels.len(), 3);
        assert_eq!(levels[0], vec!["a"]);
        assert!(levels[1].contains(&"b".to_string()));
        assert!(levels[1].contains(&"c".to_string()));
        assert_eq!(levels[2], vec!["d"]);
    }

    fn make_pkg(name: &str, deps: &[&str]) -> Package {
        Package {
            name: name.to_string(),
            version: semver::Version::new(1, 0, 0),
            path: std::path::PathBuf::from(format!("packages/{}", name)),
            manifest_path: std::path::PathBuf::from(format!("packages/{}/package.json", name)),
            is_root: false,
            local_dependencies: deps
                .iter()
                .map(|d| (d.to_string(), "^1.0.0".to_string()))
                .collect(),
            dependencies: deps
                .iter()
                .map(|d| (d.to_string(), "^1.0.0".to_string()))
                .collect(),
            dev_dependencies: HashMap::new(),
        }
    }
}
