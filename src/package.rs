use anyhow::Result;
use semver::Version;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Represents a discovered package in the repository.
/// Ecosystem-agnostic — populated by a [`crate::resolver::PackageResolver`].
#[derive(Debug, Clone)]
pub struct Package {
    /// Package name (e.g. "@acme/core", "my-lib")
    pub name: String,
    /// Current version
    pub version: Version,
    /// Path to the package directory (relative to repo root)
    pub path: PathBuf,
    /// Path to the manifest file (relative to repo root)
    pub manifest_path: PathBuf,
    /// Whether this is the root package (manifest at repo root)
    pub is_root: bool,
    /// Dependencies on other packages in this repo (name -> version requirement)
    pub local_dependencies: HashMap<String, String>,
    /// All dependencies (for reference)
    pub dependencies: HashMap<String, String>,
    /// All devDependencies (for reference)
    pub dev_dependencies: HashMap<String, String>,
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
/// Returns the most specific (longest path) matching package.
pub fn file_to_package<'a>(file_path: &str, packages: &'a [Package]) -> Option<&'a Package> {
    let file = Path::new(file_path);

    packages
        .iter()
        .filter(|pkg| {
            if pkg.path.as_os_str().is_empty() {
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

    fn make_pkg(name: &str, path: &str, local_deps: &[(&str, &str)]) -> Package {
        Package {
            name: name.into(),
            version: Version::new(1, 0, 0),
            path: PathBuf::from(path),
            manifest_path: PathBuf::from(format!(
                "{}{}package.json",
                path,
                if path.is_empty() { "" } else { "/" }
            )),
            is_root: path.is_empty(),
            local_dependencies: local_deps
                .iter()
                .map(|(n, v)| (n.to_string(), v.to_string()))
                .collect(),
            dependencies: HashMap::new(),
            dev_dependencies: HashMap::new(),
        }
    }

    #[test]
    fn test_file_to_package() {
        let packages = vec![
            make_pkg("root", "", &[]),
            make_pkg("@myorg/core", "packages/core", &[]),
            make_pkg("@myorg/utils", "packages/utils", &[]),
        ];

        assert_eq!(
            file_to_package("packages/core/src/index.ts", &packages)
                .unwrap()
                .name,
            "@myorg/core"
        );
        assert_eq!(
            file_to_package("packages/utils/lib/helpers.ts", &packages)
                .unwrap()
                .name,
            "@myorg/utils"
        );
        assert_eq!(
            file_to_package("README.md", &packages).unwrap().name,
            "root"
        );
    }

    #[test]
    fn test_topological_sort() {
        let packages = vec![
            make_pkg("a", "packages/a", &[]),
            make_pkg("b", "packages/b", &[("a", "^1.0.0")]),
            make_pkg("c", "packages/c", &[("b", "^1.0.0")]),
        ];

        let order = topological_sort(&packages).unwrap();
        let pos_a = order.iter().position(|n| n == "a").unwrap();
        let pos_b = order.iter().position(|n| n == "b").unwrap();
        let pos_c = order.iter().position(|n| n == "c").unwrap();
        assert!(pos_a < pos_b);
        assert!(pos_b < pos_c);
    }

    #[test]
    fn test_circular_dependency_detected() {
        let packages = vec![
            make_pkg("a", "packages/a", &[("b", "^1.0.0")]),
            make_pkg("b", "packages/b", &[("a", "^1.0.0")]),
        ];

        assert!(topological_sort(&packages).is_err());
    }
}
