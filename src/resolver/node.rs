use anyhow::{Context, Result};
use git2::Repository;
use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

use super::PackageResolver;
use crate::package::Package;

#[derive(Debug, Deserialize, Serialize)]
struct PackageJson {
    name: Option<String>,
    version: Option<String>,
    dependencies: Option<HashMap<String, String>>,
    #[serde(rename = "devDependencies")]
    dev_dependencies: Option<HashMap<String, String>>,
    #[serde(rename = "peerDependencies")]
    peer_dependencies: Option<HashMap<String, String>>,
    private: Option<bool>,
}

pub struct NodeResolver;

impl PackageResolver for NodeResolver {
    fn discover(&self, repo_root: &Path) -> Result<Vec<Package>> {
        let repo = Repository::open(repo_root).ok();
        let mut packages = Vec::new();
        find_package_jsons(repo_root, repo_root, repo.as_ref(), &mut packages)?;
        Ok(packages)
    }

    fn resolve_dependencies(&self, packages: &mut [Package]) {
        let names: Vec<String> = packages.iter().map(|p| p.name.clone()).collect();
        for pkg in packages.iter_mut() {
            let mut local = HashMap::new();
            for (dep_name, dep_version) in
                pkg.dependencies.iter().chain(pkg.dev_dependencies.iter())
            {
                if names.contains(dep_name) {
                    local.insert(dep_name.clone(), dep_version.clone());
                }
            }
            pkg.local_dependencies = local;
        }
    }
}

fn find_package_jsons(
    root: &Path,
    dir: &Path,
    repo: Option<&Repository>,
    packages: &mut Vec<Package>,
) -> Result<()> {
    let entries =
        std::fs::read_dir(dir).with_context(|| format!("reading dir: {}", dir.display()))?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            let dir_name = path.file_name().unwrap_or_default().to_string_lossy();
            if dir_name == ".git" {
                continue;
            }
            if is_git_ignored(repo, &path) {
                continue;
            }
            find_package_jsons(root, &path, repo, packages)?;
        } else if path.file_name().map(|f| f == "package.json").unwrap_or(false)
            && let Some(pkg) = parse_package_json(root, &path)?
        {
            packages.push(pkg);
        }
    }
    Ok(())
}

fn is_git_ignored(repo: Option<&Repository>, path: &Path) -> bool {
    repo.map(|r| r.is_path_ignored(path).unwrap_or(false))
        .unwrap_or(false)
}

fn parse_package_json(root: &Path, manifest_path: &Path) -> Result<Option<Package>> {
    let content = std::fs::read_to_string(manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?;
    let pkg_json: PackageJson = serde_json::from_str(&content)
        .with_context(|| format!("parsing {}", manifest_path.display()))?;

    let name = match pkg_json.name {
        Some(n) => n,
        None => return Ok(None),
    };

    let version = match pkg_json.version.as_deref() {
        Some(v) => Version::parse(v).unwrap_or_else(|_| Version::new(0, 0, 0)),
        None => Version::new(0, 0, 0),
    };

    let rel_manifest = manifest_path
        .strip_prefix(root)
        .unwrap_or(manifest_path)
        .to_path_buf();
    let rel_dir = rel_manifest
        .parent()
        .unwrap_or(Path::new(""))
        .to_path_buf();

    let dependencies = pkg_json.dependencies.unwrap_or_default();
    let dev_dependencies = pkg_json.dev_dependencies.unwrap_or_default();

    let is_root = rel_dir.as_os_str().is_empty();

    Ok(Some(Package {
        name,
        version,
        path: rel_dir,
        manifest_path: rel_manifest,
        is_root,
        local_dependencies: HashMap::new(),
        dependencies,
        dev_dependencies,
    }))
}
