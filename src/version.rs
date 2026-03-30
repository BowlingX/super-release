use std::collections::HashMap;

use anyhow::Result;
use git2::Repository;
use semver::Version;

use crate::commit::{BumpLevel, ConventionalCommit};
use crate::git;
use crate::package::{file_to_package, Package};

/// The release plan for a single package.
#[derive(Debug, Clone)]
pub struct PackageRelease {
    pub package_name: String,
    pub current_version: Version,
    pub next_version: Version,
    pub bump: BumpLevel,
    pub commits: Vec<ConventionalCommit>,
}

/// Determine the next version for all packages based on commits since their last release.
pub fn determine_releases(
    repo: &Repository,
    packages: &[Package],
) -> Result<Vec<PackageRelease>> {
    let mut releases = Vec::new();

    for pkg in packages {
        let release = determine_package_release(repo, pkg, packages)?;
        if release.bump > BumpLevel::None {
            releases.push(release);
        }
    }

    Ok(releases)
}

/// Determine the release for a single package.
fn determine_package_release(
    repo: &Repository,
    pkg: &Package,
    all_packages: &[Package],
) -> Result<PackageRelease> {
    // Find the latest version tag for this package
    let latest = git::find_latest_version(repo, &pkg.name)?;
    let (since_tag, current_version) = match &latest {
        Some((tag, ver)) => (Some(tag.as_str()), ver.clone()),
        None => (None, pkg.version.clone()),
    };

    // Get all commits since that tag
    let all_commits = git::get_commits_since(repo, since_tag)?;

    // Filter commits that touch this package
    let pkg_commits: Vec<ConventionalCommit> = all_commits
        .into_iter()
        .filter(|c| {
            c.files_changed.iter().any(|f| {
                if let Some(matched_pkg) = file_to_package(f, all_packages) {
                    matched_pkg.name == pkg.name
                } else {
                    false
                }
            })
        })
        .collect();

    // Determine the highest bump level
    let bump = pkg_commits
        .iter()
        .map(|c| c.bump)
        .max()
        .unwrap_or(BumpLevel::None);

    let next_version = apply_bump(&current_version, bump);

    Ok(PackageRelease {
        package_name: pkg.name.clone(),
        current_version,
        next_version,
        bump,
        commits: pkg_commits,
    })
}

/// Apply a bump level to a version.
pub fn apply_bump(version: &Version, bump: BumpLevel) -> Version {
    match bump {
        BumpLevel::None => version.clone(),
        BumpLevel::Patch => Version::new(version.major, version.minor, version.patch + 1),
        BumpLevel::Minor => Version::new(version.major, version.minor + 1, 0),
        BumpLevel::Major => {
            if version.major == 0 {
                // For 0.x.y, a breaking change bumps minor (common convention)
                Version::new(0, version.minor + 1, 0)
            } else {
                Version::new(version.major + 1, 0, 0)
            }
        }
    }
}

/// Group commits by package, returning a map of package name -> commits.
pub fn group_commits_by_package(
    commits: &[ConventionalCommit],
    packages: &[Package],
) -> HashMap<String, Vec<ConventionalCommit>> {
    let mut grouped: HashMap<String, Vec<ConventionalCommit>> = HashMap::new();

    for commit in commits {
        for file in &commit.files_changed {
            if let Some(pkg) = file_to_package(file, packages) {
                grouped
                    .entry(pkg.name.clone())
                    .or_default()
                    .push(commit.clone());
                break; // Don't double-count a commit for the same package
            }
        }
    }

    grouped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_apply_bump_patch() {
        let v = Version::new(1, 2, 3);
        assert_eq!(apply_bump(&v, BumpLevel::Patch), Version::new(1, 2, 4));
    }

    #[test]
    fn test_apply_bump_minor() {
        let v = Version::new(1, 2, 3);
        assert_eq!(apply_bump(&v, BumpLevel::Minor), Version::new(1, 3, 0));
    }

    #[test]
    fn test_apply_bump_major() {
        let v = Version::new(1, 2, 3);
        assert_eq!(apply_bump(&v, BumpLevel::Major), Version::new(2, 0, 0));
    }

    #[test]
    fn test_apply_bump_major_zero() {
        let v = Version::new(0, 2, 3);
        assert_eq!(apply_bump(&v, BumpLevel::Major), Version::new(0, 3, 0));
    }

    #[test]
    fn test_apply_bump_none() {
        let v = Version::new(1, 2, 3);
        assert_eq!(apply_bump(&v, BumpLevel::None), Version::new(1, 2, 3));
    }
}
