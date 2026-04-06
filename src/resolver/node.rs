use anyhow::{Context, Result};
use git2::Repository;
use regex::Regex;
use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use super::PackageResolver;
use crate::package::Package;
use crate::version::PackageRelease;

#[derive(Debug, Deserialize, Serialize)]
struct PackageJson {
    name: Option<String>,
    version: Option<String>,
    dependencies: Option<HashMap<String, String>>,
    #[serde(rename = "devDependencies")]
    dev_dependencies: Option<HashMap<String, String>>,
    #[serde(rename = "optionalDependencies")]
    optional_dependencies: Option<HashMap<String, String>>,
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
        let names: std::collections::HashSet<String> =
            packages.iter().map(|p| p.name.clone()).collect();
        for pkg in packages.iter_mut() {
            let mut local = HashMap::new();
            for (dep_name, dep_version) in pkg
                .dependencies
                .iter()
                .chain(pkg.dev_dependencies.iter())
                .chain(pkg.optional_dependencies.iter())
            {
                if names.contains(dep_name) {
                    local.insert(dep_name.clone(), dep_version.clone());
                }
            }
            pkg.local_dependencies = local;
        }
    }

    fn bump_versions(
        &self,
        repo_root: &Path,
        packages: &[Package],
        releases: &[PackageRelease],
        dry_run: bool,
    ) -> Result<Vec<PathBuf>> {
        let mut modified = Vec::new();

        for release in releases {
            let pkg = packages
                .iter()
                .find(|p| p.name == release.package_name)
                .context(format!("package '{}' not found", release.package_name))?;
            let manifest_path = repo_root.join(&pkg.manifest_path);

            if dry_run {
                println!(
                    "  [version] Would update {}: {} -> {}",
                    pkg.manifest_path.display(),
                    release.current_version,
                    release.next_version
                );
            } else {
                update_package_version(&manifest_path, &release.next_version)?;
                println!(
                    "  [version] Updated {} to {}",
                    pkg.manifest_path.display(),
                    release.next_version
                );
            }
            modified.push(pkg.manifest_path.clone());
        }

        Ok(modified)
    }
}

fn update_package_version(path: &Path, new_version: &semver::Version) -> Result<()> {
    let content =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;

    static VERSION_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"("version"\s*:\s*)"[^"]*""#)
            .expect("version regex is invalid — this is a bug")
    });

    // Replace only the "version" field value, preserving all other formatting.
    let re = &*VERSION_RE;
    let replacement = format!(r#"${{1}}"{}""#, new_version);
    let updated = re.replace(&content, replacement.as_str());

    if updated == content {
        anyhow::bail!("Could not find \"version\" field in {}", path.display());
    }

    std::fs::write(path, updated.as_bytes())?;
    Ok(())
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
        } else if path
            .file_name()
            .map(|f| f == "package.json")
            .unwrap_or(false)
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

    let rel_manifest = manifest_path
        .strip_prefix(root)
        .unwrap_or(manifest_path)
        .to_path_buf();
    let rel_dir = rel_manifest.parent().unwrap_or(Path::new("")).to_path_buf();
    let is_root = rel_dir.as_os_str().is_empty();

    let parsed_version = pkg_json.version.as_deref().map(Version::parse);
    let (version, version_warning) = match &parsed_version {
        Some(Ok(v)) => (v.clone(), None),
        Some(Err(e)) => (
            Version::new(0, 0, 0),
            Some(format!(
                "invalid version \"{}\": {}, defaulting to 0.0.0",
                pkg_json.version.as_deref().unwrap_or(""),
                e
            )),
        ),
        None => (
            Version::new(0, 0, 0),
            Some("no \"version\" field, defaulting to 0.0.0".to_string()),
        ),
    };

    let name = match pkg_json.name {
        Some(n) => n,
        None => {
            let display_path = if is_root {
                ".".to_string()
            } else {
                rel_dir.display().to_string()
            };
            return Ok(Some(Package {
                name: display_path,
                version: Version::new(0, 0, 0),
                path: rel_dir,
                manifest_path: rel_manifest,
                is_root,
                local_dependencies: HashMap::new(),
                dependencies: HashMap::new(),
                dev_dependencies: HashMap::new(),
                optional_dependencies: HashMap::new(),
                warning: Some("no \"name\" field, skipped".to_string()),
                skipped: true,
            }));
        }
    };

    let dependencies = pkg_json.dependencies.unwrap_or_default();
    let dev_dependencies = pkg_json.dev_dependencies.unwrap_or_default();
    let optional_dependencies = pkg_json.optional_dependencies.unwrap_or_default();

    Ok(Some(Package {
        name,
        version,
        path: rel_dir,
        manifest_path: rel_manifest,
        is_root,
        local_dependencies: HashMap::new(),
        dependencies,
        dev_dependencies,
        optional_dependencies,
        warning: version_warning,
        skipped: false,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_update_package_version() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("package.json");
        let original =
            r#"{"name":"@acme/core","version":"1.0.0","dependencies":{"@acme/utils":"^1.0.0"}}"#;
        std::fs::write(&path, original).unwrap();

        update_package_version(&path, &semver::Version::new(1, 1, 0)).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        // Exact formatting preserved, only version value changed
        assert_eq!(
            content,
            r#"{"name":"@acme/core","version":"1.1.0","dependencies":{"@acme/utils":"^1.0.0"}}"#
        );
    }

    #[test]
    fn test_update_preserves_formatting() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("package.json");
        let original =
            "{\n  \"name\": \"my-pkg\",\n  \"version\": \"1.0.0\",\n  \"private\": true\n}\n";
        std::fs::write(&path, original).unwrap();

        update_package_version(&path, &semver::Version::new(2, 0, 0)).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            content,
            "{\n  \"name\": \"my-pkg\",\n  \"version\": \"2.0.0\",\n  \"private\": true\n}\n"
        );
    }

    #[test]
    fn test_update_preserves_workspace_protocol() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("package.json");
        let original =
            r#"{"name":"@acme/app","version":"1.0.0","dependencies":{"@acme/core":"workspace:*"}}"#;
        std::fs::write(&path, original).unwrap();

        update_package_version(&path, &semver::Version::new(2, 0, 0)).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            content,
            r#"{"name":"@acme/app","version":"2.0.0","dependencies":{"@acme/core":"workspace:*"}}"#
        );
    }

    #[test]
    fn test_parse_missing_name_returns_warning() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("package.json");
        std::fs::write(&path, r#"{"version": "1.0.0"}"#).unwrap();

        let result = parse_package_json(dir.path(), &path).unwrap().unwrap();
        assert!(result.skipped);
        assert!(
            result
                .warning
                .as_ref()
                .unwrap()
                .contains("no \"name\" field")
        );
    }

    #[test]
    fn test_parse_invalid_version_returns_warning() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("package.json");
        std::fs::write(&path, r#"{"name": "my-pkg", "version": "not-semver"}"#).unwrap();

        let result = parse_package_json(dir.path(), &path).unwrap().unwrap();
        assert_eq!(result.name, "my-pkg");
        assert_eq!(result.version, Version::new(0, 0, 0));
        assert!(result.warning.as_ref().unwrap().contains("invalid version"));
    }

    #[test]
    fn test_parse_missing_version_returns_warning() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("package.json");
        std::fs::write(&path, r#"{"name": "my-pkg"}"#).unwrap();

        let result = parse_package_json(dir.path(), &path).unwrap().unwrap();
        assert_eq!(result.name, "my-pkg");
        assert_eq!(result.version, Version::new(0, 0, 0));
        assert!(
            result
                .warning
                .as_ref()
                .unwrap()
                .contains("no \"version\" field")
        );
    }

    #[test]
    fn test_parse_valid_package_no_warning() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("package.json");
        std::fs::write(&path, r#"{"name": "my-pkg", "version": "1.2.3"}"#).unwrap();

        let result = parse_package_json(dir.path(), &path).unwrap().unwrap();
        assert_eq!(result.name, "my-pkg");
        assert_eq!(result.version, Version::new(1, 2, 3));
        assert!(result.warning.is_none());
    }
}
