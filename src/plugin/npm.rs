use anyhow::Result;
use rayon::prelude::*;
use serde::Deserialize;
use std::collections::HashMap;

use super::{parse_options, subprocess, Plugin, PluginConfig, PluginContext};
use crate::package::{topological_sort, Package};
use crate::pm::PackageManager;
use crate::version::PackageRelease;

/// Options for the npm/publish plugin.
#[derive(Debug, Clone, Deserialize)]
pub struct NpmOptions {
    /// Access level for publish (default: "public")
    #[serde(default = "default_access")]
    pub access: String,

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

impl Default for NpmOptions {
    fn default() -> Self {
        Self {
            access: default_access(),
            registry: None,
            publish_args: Vec::new(),
            tag: None,
            provenance: false,
            package_manager: None,
        }
    }
}

fn default_access() -> String {
    "public".into()
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
            let results: Vec<Result<()>> = if level.len() == 1 {
                level
                    .iter()
                    .filter_map(|name| {
                        let release = release_set.get(name.as_str())?;
                        let pkg = packages.iter().find(|p| p.name == *name)?;
                        Some(publish_one(repo_root, dry_run, pkg, release, &opts, pm, dist_tag))
                    })
                    .collect()
            } else {
                level
                    .par_iter()
                    .filter_map(|name| {
                        let release = release_set.get(name.as_str())?;
                        let pkg = packages.iter().find(|p| p.name == *name)?;
                        Some(publish_one(repo_root, dry_run, pkg, release, &opts, pm, dist_tag))
                    })
                    .collect()
            };

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

    let cmd = pm.publish_command(
        &pkg_dir,
        &opts.access,
        opts.registry.as_deref(),
        dist_tag,
        opts.provenance,
        &opts.publish_args,
    );

    if dry_run {
        println!("  [{}] Would publish {}: {}", pm, label, subprocess::format_command(&cmd));
        return Ok(());
    }

    println!("  [{}] Publishing {}: {}", pm, label, subprocess::format_command(&cmd));
    subprocess::run_command(cmd, &label, &pm.to_string())
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
            local_dependencies: deps.iter().map(|d| (d.to_string(), "^1.0.0".to_string())).collect(),
            dependencies: deps.iter().map(|d| (d.to_string(), "^1.0.0".to_string())).collect(),
            dev_dependencies: HashMap::new(),
        }
    }
}
