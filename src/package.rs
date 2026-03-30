use anyhow::{Context, Result};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Represents a discovered package in the repository.
#[derive(Debug, Clone)]
pub struct Package {
    /// Name from package.json
    pub name: String,
    /// Current version from package.json
    pub version: Version,
    /// Path to the package directory (relative to repo root)
    pub path: PathBuf,
    /// Path to the package.json file (relative to repo root)
    pub manifest_path: PathBuf,
    /// Whether this is the root package (package.json at repo root)
    pub is_root: bool,
    /// Dependencies on other packages in this repo (name -> version requirement)
    pub local_dependencies: HashMap<String, String>,
    /// All dependencies (for reference)
    pub dependencies: HashMap<String, String>,
    /// All devDependencies (for reference)
    pub dev_dependencies: HashMap<String, String>,
}

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

/// Discover all packages in the repository by finding package.json files.
/// Skips node_modules directories.
pub fn discover_packages(repo_root: &Path) -> Result<Vec<Package>> {
    let mut packages = Vec::new();
    find_package_jsons(repo_root, repo_root, &mut packages)?;
    Ok(packages)
}

fn find_package_jsons(root: &Path, dir: &Path, packages: &mut Vec<Package>) -> Result<()> {
    let entries = std::fs::read_dir(dir).with_context(|| format!("reading dir: {}", dir.display()))?;

    for entry in entries {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            let dir_name = path.file_name().unwrap_or_default().to_string_lossy();
            // Skip common non-package directories
            if dir_name == "node_modules" || dir_name == ".git" || dir_name == "dist" || dir_name == "build" {
                continue;
            }
            find_package_jsons(root, &path, packages)?;
        } else if path.file_name().map(|f| f == "package.json").unwrap_or(false) {
            if let Some(pkg) = parse_package_json(root, &path)? {
                packages.push(pkg);
            }
        }
    }
    Ok(())
}

fn parse_package_json(root: &Path, manifest_path: &Path) -> Result<Option<Package>> {
    let content = std::fs::read_to_string(manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?;
    let pkg_json: PackageJson =
        serde_json::from_str(&content).with_context(|| format!("parsing {}", manifest_path.display()))?;

    let name = match pkg_json.name {
        Some(n) => n,
        None => return Ok(None), // Skip unnamed packages
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
        local_dependencies: HashMap::new(), // Resolved later
        dependencies,
        dev_dependencies,
    }))
}

/// Resolve local (in-repo) dependencies between packages.
pub fn resolve_local_dependencies(packages: &mut [Package]) {
    let names: Vec<String> = packages.iter().map(|p| p.name.clone()).collect();
    for pkg in packages.iter_mut() {
        let mut local = HashMap::new();
        for (dep_name, dep_version) in pkg.dependencies.iter().chain(pkg.dev_dependencies.iter()) {
            if names.contains(dep_name) {
                local.insert(dep_name.clone(), dep_version.clone());
            }
        }
        pkg.local_dependencies = local;
    }
}

/// Build a topological ordering of packages based on local dependencies.
/// Returns packages in order such that dependencies come before dependents.
pub fn topological_sort(packages: &[Package]) -> Result<Vec<String>> {
    let name_set: HashMap<&str, usize> = packages
        .iter()
        .enumerate()
        .map(|(i, p)| (p.name.as_str(), i))
        .collect();

    let n = packages.len();
    let mut in_degree = vec![0usize; n];
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];

    for (i, pkg) in packages.iter().enumerate() {
        for dep_name in pkg.local_dependencies.keys() {
            if let Some(&j) = name_set.get(dep_name.as_str()) {
                adj[j].push(i);
                in_degree[i] += 1;
            }
        }
    }

    let mut queue: Vec<usize> = (0..n).filter(|&i| in_degree[i] == 0).collect();
    let mut order = Vec::new();

    while let Some(node) = queue.pop() {
        order.push(packages[node].name.clone());
        for &next in &adj[node] {
            in_degree[next] -= 1;
            if in_degree[next] == 0 {
                queue.push(next);
            }
        }
    }

    if order.len() != n {
        anyhow::bail!("Circular dependency detected among packages");
    }

    Ok(order)
}

/// Determine which package a file belongs to.
/// Returns the package name if the file path starts with the package's directory.
pub fn file_to_package<'a>(file_path: &str, packages: &'a [Package]) -> Option<&'a Package> {
    let file = Path::new(file_path);

    // Find the most specific (longest path) matching package
    packages
        .iter()
        .filter(|pkg| {
            if pkg.path.as_os_str().is_empty() {
                // Root package matches everything not matched by others
                true
            } else {
                file.starts_with(&pkg.path)
            }
        })
        .max_by_key(|pkg| pkg.path.components().count())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_to_package() {
        let packages = vec![
            Package {
                name: "root".into(),
                version: Version::new(1, 0, 0),
                path: PathBuf::from(""),
                manifest_path: PathBuf::from("package.json"),
                is_root: true,
                local_dependencies: HashMap::new(),
                dependencies: HashMap::new(),
                dev_dependencies: HashMap::new(),
            },
            Package {
                name: "@myorg/core".into(),
                version: Version::new(1, 0, 0),
                path: PathBuf::from("packages/core"),
                manifest_path: PathBuf::from("packages/core/package.json"),
                is_root: false,
                local_dependencies: HashMap::new(),
                dependencies: HashMap::new(),
                dev_dependencies: HashMap::new(),
            },
            Package {
                name: "@myorg/utils".into(),
                version: Version::new(1, 0, 0),
                path: PathBuf::from("packages/utils"),
                manifest_path: PathBuf::from("packages/utils/package.json"),
                is_root: false,
                local_dependencies: HashMap::new(),
                dependencies: HashMap::new(),
                dev_dependencies: HashMap::new(),
            },
        ];

        assert_eq!(
            file_to_package("packages/core/src/index.ts", &packages).unwrap().name,
            "@myorg/core"
        );
        assert_eq!(
            file_to_package("packages/utils/lib/helpers.ts", &packages).unwrap().name,
            "@myorg/utils"
        );
        // Root-level file goes to root package
        assert_eq!(
            file_to_package("README.md", &packages).unwrap().name,
            "root"
        );
    }

    #[test]
    fn test_topological_sort() {
        let packages = vec![
            Package {
                name: "a".into(),
                version: Version::new(1, 0, 0),
                path: PathBuf::from("packages/a"),
                manifest_path: PathBuf::from("packages/a/package.json"),
                is_root: false,
                local_dependencies: HashMap::new(),
                dependencies: HashMap::new(),
                dev_dependencies: HashMap::new(),
            },
            Package {
                name: "b".into(),
                version: Version::new(1, 0, 0),
                path: PathBuf::from("packages/b"),
                manifest_path: PathBuf::from("packages/b/package.json"),
                is_root: false,
                local_dependencies: [("a".into(), "^1.0.0".into())].into_iter().collect(),
                dependencies: HashMap::new(),
                dev_dependencies: HashMap::new(),
            },
            Package {
                name: "c".into(),
                version: Version::new(1, 0, 0),
                path: PathBuf::from("packages/c"),
                manifest_path: PathBuf::from("packages/c/package.json"),
                is_root: false,
                local_dependencies: [("b".into(), "^1.0.0".into())].into_iter().collect(),
                dependencies: HashMap::new(),
                dev_dependencies: HashMap::new(),
            },
        ];

        let order = topological_sort(&packages).unwrap();
        let pos_a = order.iter().position(|n| n == "a").unwrap();
        let pos_b = order.iter().position(|n| n == "b").unwrap();
        let pos_c = order.iter().position(|n| n == "c").unwrap();
        assert!(pos_a < pos_b);
        assert!(pos_b < pos_c);
    }

    #[test]
    fn test_resolve_local_dependencies() {
        let mut packages = vec![
            Package {
                name: "core".into(),
                version: Version::new(1, 0, 0),
                path: PathBuf::from("packages/core"),
                manifest_path: PathBuf::from("packages/core/package.json"),
                is_root: false,
                local_dependencies: HashMap::new(),
                dependencies: HashMap::new(),
                dev_dependencies: HashMap::new(),
            },
            Package {
                name: "app".into(),
                version: Version::new(1, 0, 0),
                path: PathBuf::from("packages/app"),
                manifest_path: PathBuf::from("packages/app/package.json"),
                is_root: false,
                local_dependencies: HashMap::new(),
                dependencies: [("core".into(), "^1.0.0".into()), ("lodash".into(), "^4.0.0".into())].into_iter().collect(),
                dev_dependencies: HashMap::new(),
            },
        ];

        resolve_local_dependencies(&mut packages);
        assert!(packages[0].local_dependencies.is_empty());
        assert_eq!(packages[1].local_dependencies.len(), 1);
        assert!(packages[1].local_dependencies.contains_key("core"));
    }
}
