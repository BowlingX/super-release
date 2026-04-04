use anyhow::Result;
use rayon::prelude::*;
use serde::Deserialize;
use std::collections::HashMap;

use super::{Step, StepConfig, StepContext, parse_options, subprocess};
use crate::package::{Package, topological_sort};
use crate::pm::PackageManager;
use crate::version::PackageRelease;

/// Options for the npm/publish step.
#[derive(Debug, Clone, Deserialize)]
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

    /// Check the registry before publishing to skip already-published versions.
    /// Default: true. Set to false to skip the check and always attempt publish.
    #[serde(default = "default_check_registry")]
    pub check_registry: bool,
}

fn default_check_registry() -> bool {
    true
}

impl Default for NpmOptions {
    fn default() -> Self {
        Self {
            access: None,
            registry: None,
            publish_args: Vec::new(),
            tag: None,
            provenance: false,
            package_manager: None,
            check_registry: true,
        }
    }
}

pub struct NpmStep;

impl Step for NpmStep {
    fn name(&self) -> &str {
        "npm"
    }

    fn verify(&self, ctx: &StepContext, config: &StepConfig) -> Result<()> {
        if ctx.dry_run {
            return Ok(());
        }
        let opts: NpmOptions = parse_options(config)?;
        resolve_pm(ctx, &opts)?.verify()
    }

    fn publish(
        &self,
        ctx: &StepContext,
        config: &StepConfig,
        packages: &[Package],
        releases: &[PackageRelease],
    ) -> Result<Vec<std::path::PathBuf>> {
        let opts: NpmOptions = parse_options(config)?;
        let pm = resolve_pm(ctx, &opts)?;
        let dist_tag: Option<&str> = opts.tag.as_deref().or(ctx.branch.channel.as_deref());

        let order = topological_sort(packages)?;
        let release_set: HashMap<&str, &PackageRelease> = releases
            .iter()
            .map(|r| (r.package_name.as_str(), r))
            .collect();

        let repo_root = ctx.repo_root;
        let dry_run = ctx.dry_run;
        let mut errors: Vec<String> = Vec::new();

        // Check all packages against the registry in parallel
        let to_publish: HashMap<&str, &PackageRelease> = if opts.check_registry {
            println!("  [{}] Checking registry for published versions...", pm);

            let check_results: Vec<(&str, VersionCheckResult)> = releases
                .par_iter()
                .map(|r| {
                    let version_str = r.next_version.to_string();
                    let view_cmd_str = format_view_command(
                        &r.package_name,
                        &version_str,
                        opts.registry.as_deref(),
                    );
                    println!(
                        "    {} {}",
                        console::style(&r.package_name).bold(),
                        console::style(&view_cmd_str).dim()
                    );
                    let result =
                        run_version_check(&r.package_name, &version_str, opts.registry.as_deref());
                    (r.package_name.as_str(), result)
                })
                .collect();

            let mut to_publish = HashMap::new();
            for (name, result) in &check_results {
                let display = match result {
                    VersionCheckResult::Published(v) => format!("→ {} (published)", v),
                    VersionCheckResult::NotFound(v) => format!("→ {} (not found)", v),
                    VersionCheckResult::Error(e) => format!("→ error: {}", e),
                };
                println!(
                    "    {} {}",
                    console::style(name).bold(),
                    console::style(&display).dim()
                );

                match result {
                    VersionCheckResult::Published(_) => {
                        let ver = release_set
                            .get(name)
                            .map(|r| r.next_version.to_string())
                            .unwrap_or_default();
                        println!("  [{}] {} v{} already published, skipping", pm, name, ver);
                    }
                    VersionCheckResult::NotFound(_) => {
                        if let Some(&r) = release_set.get(name) {
                            to_publish.insert(*name, r);
                        }
                    }
                    VersionCheckResult::Error(e) => {
                        errors.push(format!("Registry check failed for {}: {}", name, e));
                    }
                }
            }

            if !errors.is_empty() {
                anyhow::bail!("Registry check failed:\n  {}", errors.join("\n  "));
            }

            to_publish
        } else {
            release_set.clone()
        };

        if to_publish.is_empty() {
            println!("  [{}] All packages already published", pm);
            return Ok(Vec::new());
        }

        // Show publish order
        let publish_levels = dependency_levels(&order, packages, &to_publish);
        if publish_levels.len() > 1 || publish_levels.first().map(|l| l.len() > 1).unwrap_or(false)
        {
            println!("  [{}] Publish order:", pm);
            for (i, level) in publish_levels.iter().enumerate() {
                if !level.is_empty() {
                    let parallel = if level.len() > 1 { " (parallel)" } else { "" };
                    println!(
                        "    {} {}{}",
                        console::style(format!("{}.", i + 1)).dim(),
                        level.join(", "),
                        console::style(parallel).dim()
                    );
                }
            }
        }

        for level in &publish_levels {
            let results: Vec<Result<()>> = level
                .par_iter()
                .filter_map(|name| {
                    let release = to_publish.get(name.as_str())?;
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

fn resolve_pm(ctx: &StepContext, opts: &NpmOptions) -> Result<PackageManager> {
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

    let cmd = pm.publish_command(
        &pkg_dir,
        opts.access.as_deref(),
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
            step_name: &pm_name,
        },
    )
}

/// Format the npm view command for display.
fn format_view_command(name: &str, version: &str, registry: Option<&str>) -> String {
    let mut cmd = std::process::Command::new("npm");
    cmd.args(["view", &format!("{}@{}", name, version), "version"]);
    if let Some(reg) = registry {
        cmd.args(["--registry", reg]);
    }
    subprocess::format_command(&cmd)
}

enum VersionCheckResult {
    /// Version exists on the registry.
    Published(String),
    /// 404: version not found (safe to publish).
    NotFound(String),
    /// Registry error (auth, network, etc.) — should not proceed.
    Error(String),
}

fn run_version_check(name: &str, version: &str, registry: Option<&str>) -> VersionCheckResult {
    let mut cmd = std::process::Command::new("npm");
    cmd.args(["view", &format!("{}@{}", name, version), "version"]);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    if let Some(reg) = registry {
        cmd.args(["--registry", reg]);
    }

    let Ok(output) = cmd.output() else {
        return VersionCheckResult::Error("failed to run npm view".into());
    };

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    if output.status.success() {
        if stdout == version {
            return VersionCheckResult::Published(stdout);
        }
        // Version not matched — treat as not found
        return VersionCheckResult::NotFound(stdout);
    }

    if stderr.contains("E404") {
        VersionCheckResult::NotFound(stderr)
    } else {
        VersionCheckResult::Error(stderr)
    }
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
            warning: None,
            skipped: false,
        }
    }
}
