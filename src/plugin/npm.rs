use anyhow::{Context, Result};
use std::collections::HashMap;
use std::fs;
use std::process::Command;

use super::{Plugin, PluginConfig, PluginContext};
use crate::package::{topological_sort, Package};
use crate::version::PackageRelease;

pub struct NpmPlugin;

impl Plugin for NpmPlugin {
    fn name(&self) -> &str {
        "npm"
    }

    fn verify(&self, ctx: &PluginContext, _config: &PluginConfig) -> Result<()> {
        if ctx.dry_run {
            return Ok(());
        }

        // Check that npm is available
        let output = Command::new("npm").arg("--version").output();
        match output {
            Ok(o) if o.status.success() => Ok(()),
            _ => anyhow::bail!("npm is not available. Please install Node.js/npm."),
        }
    }

    fn prepare(
        &self,
        ctx: &PluginContext,
        _config: &PluginConfig,
        packages: &[Package],
        releases: &[PackageRelease],
    ) -> Result<()> {
        // Build a map of package name -> next version for released packages
        let version_map: HashMap<&str, &semver::Version> = releases
            .iter()
            .map(|r| (r.package_name.as_str(), &r.next_version))
            .collect();

        // Update package.json files in topological order
        let order = topological_sort(packages)?;

        for pkg_name in &order {
            if let Some(release) = releases.iter().find(|r| &r.package_name == pkg_name) {
                let pkg = packages.iter().find(|p| &p.name == pkg_name).unwrap();
                let manifest_path = ctx.repo_root.join(&pkg.manifest_path);

                if ctx.dry_run {
                    println!(
                        "  [npm] Would update {}: {} -> {}",
                        pkg.manifest_path.display(),
                        release.current_version,
                        release.next_version
                    );
                    // Show interdependency updates
                    for dep_name in pkg.local_dependencies.keys() {
                        if let Some(new_ver) = version_map.get(dep_name.as_str()) {
                            println!(
                                "  [npm] Would update dependency {} -> {} in {}",
                                dep_name, new_ver, pkg.manifest_path.display()
                            );
                        }
                    }
                    continue;
                }

                update_package_json(&manifest_path, &release.next_version, &version_map)?;
                println!(
                    "  [npm] Updated {} to {}",
                    pkg.manifest_path.display(),
                    release.next_version
                );
            }
        }

        Ok(())
    }

    fn publish(
        &self,
        ctx: &PluginContext,
        _config: &PluginConfig,
        packages: &[Package],
        releases: &[PackageRelease],
    ) -> Result<()> {
        let order = topological_sort(packages)?;

        for pkg_name in &order {
            if let Some(release) = releases.iter().find(|r| &r.package_name == pkg_name) {
                let pkg = packages.iter().find(|p| &p.name == pkg_name).unwrap();
                let pkg_dir = ctx.repo_root.join(&pkg.path);

                if ctx.dry_run {
                    println!(
                        "  [npm] Would publish {} v{} from {}",
                        pkg.name, release.next_version, pkg_dir.display()
                    );
                    continue;
                }

                println!(
                    "  [npm] Publishing {} v{} ...",
                    pkg.name, release.next_version
                );

                let output = Command::new("npm")
                    .arg("publish")
                    .arg("--access")
                    .arg("public")
                    .current_dir(&pkg_dir)
                    .output()
                    .with_context(|| format!("Failed to run npm publish for {}", pkg.name))?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    anyhow::bail!("npm publish failed for {}: {}", pkg.name, stderr);
                }

                println!("  [npm] Published {} v{}", pkg.name, release.next_version);
            }
        }

        Ok(())
    }
}

/// Update a package.json file with a new version and updated local dependency versions.
fn update_package_json(
    path: &std::path::Path,
    new_version: &semver::Version,
    version_map: &HashMap<&str, &semver::Version>,
) -> Result<()> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;

    let mut pkg: serde_json::Value = serde_json::from_str(&content)
        .with_context(|| format!("parsing {}", path.display()))?;

    pkg["version"] = serde_json::Value::String(new_version.to_string());

    for dep_field in &["dependencies", "devDependencies", "peerDependencies"] {
        if let Some(deps) = pkg.get_mut(*dep_field).and_then(|d| d.as_object_mut()) {
            for (dep_name, dep_ver) in deps.iter_mut() {
                if let Some(new_ver) = version_map.get(dep_name.as_str()) {
                    let current = dep_ver.as_str().unwrap_or("");
                    let prefix = if current.starts_with('^') {
                        "^"
                    } else if current.starts_with('~') {
                        "~"
                    } else if current.starts_with(">=") {
                        ">="
                    } else {
                        ""
                    };
                    *dep_ver =
                        serde_json::Value::String(format!("{}{}", prefix, new_ver));
                }
            }
        }
    }

    let output = serde_json::to_string_pretty(&pkg)?;
    fs::write(path, format!("{}\n", output))?;
    Ok(())
}
