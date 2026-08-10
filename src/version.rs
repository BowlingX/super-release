use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::Result;
use git2::Repository;
use rayon::prelude::*;
use semver::{Prerelease, Version};

use crate::commit::{BumpLevel, ConventionalCommit};
use crate::config::{BranchContext, Config, MaintenanceRange};
use crate::git;
use crate::package::{Package, file_to_package};

/// The release plan for a single package.
#[derive(Debug, Clone)]
pub struct PackageRelease {
    pub package_name: String,
    pub current_version: Version,
    pub next_version: Version,
    pub bump: BumpLevel,
    pub commits: Vec<ConventionalCommit>,
    pub is_root: bool,
    /// If this release was triggered by a dependency update rather than direct
    /// commits, contains the dependency chain that caused the propagation.
    pub propagated_from: Option<String>,
}

struct PkgTagInfo {
    /// The version to use as the base for calculating the next version.
    current_version: Version,
    /// The OID to stop commit walking at — may differ from the tag that
    /// produced `current_version` on prerelease branches.
    cutoff_oid: Option<git2::Oid>,
    cutoff_tag: Option<String>,
    /// When true, the cutoff commit itself is included (used for first-release
    /// packages where the cutoff is the introduction commit, not a release tag).
    cutoff_inclusive: bool,
}

/// Determine the next version for all packages based on commits since their last release.
///
/// Resolves tags first to find the oldest boundary, then only walks commits
/// from HEAD to that boundary — avoids parsing the entire git history.
pub fn determine_releases(
    repo: &Repository,
    repo_path: &Path,
    packages: &[Package],
    config: &Config,
    branch_ctx: &BranchContext,
) -> Result<Vec<PackageRelease>> {
    let pkg_pairs: Vec<(String, bool)> = packages
        .iter()
        .map(|p| (p.name.clone(), p.is_root))
        .collect();
    let tag_index = git::TagIndex::build(repo, &pkg_pairs, config, branch_ctx)?;

    let tag_infos: Vec<PkgTagInfo> = packages
        .iter()
        .map(|pkg| {
            let latest = tag_index.latest_version(&pkg.name);

            // On prerelease branches the cutoff must be this channel's own tag, not the
            // global latest which may sit on another branch and include commits we still need.
            let channel_tag = branch_ctx
                .prerelease
                .as_ref()
                .and_then(|ch| tag_index.latest_channel_version(&pkg.name, ch));

            let cutoff = channel_tag.as_ref().or(latest.as_ref());

            match cutoff {
                Some((tag_name, _)) => {
                    let oid = git::tag_to_oid(repo, tag_name)?;
                    let current_version = match (&latest, &channel_tag) {
                        (Some((_, lv)), Some((_, cv))) => lv.max(cv).clone(),
                        (Some((_, v)), None) | (None, Some((_, v))) => v.clone(),
                        (None, None) => unreachable!(),
                    };
                    Ok(PkgTagInfo {
                        current_version,
                        cutoff_oid: oid,
                        cutoff_tag: Some(tag_name.clone()),
                        cutoff_inclusive: false,
                    })
                }
                None => {
                    // First release: use the manifest's introduction commit as cutoff so we
                    // don't attribute the entire repo history to this new package.
                    let intro_oid =
                        git::find_file_introduction_oid(repo, repo_path, &pkg.manifest_path);
                    Ok(PkgTagInfo {
                        current_version: pkg.version.clone(),
                        cutoff_oid: intro_oid,
                        cutoff_tag: None,
                        cutoff_inclusive: true,
                    })
                }
            }
        })
        .collect::<Result<Vec<_>>>()?;

    // Only walk commits since the oldest tag; if any package has no tag (first
    // release), we must walk the full history.
    let all_have_tags = tag_infos.iter().all(|t| t.cutoff_tag.is_some());
    let oldest_tag: Option<&str> = if all_have_tags {
        find_oldest_tag(repo, &tag_infos)?
    } else {
        None
    };

    let mut all_commits = git::get_commits_since(repo, repo_path, oldest_tag)?;

    // A commit touching a file that matches a global dependency pattern affects ALL packages.
    let has_ignore = !config.ignore.is_empty();
    let all_pkg_names: Vec<&str> = packages.iter().map(|p| p.name.as_str()).collect();
    let mut pkg_commit_indices: HashMap<&str, Vec<usize>> = HashMap::new();

    for (i, c) in all_commits.iter().enumerate() {
        let relevant_files: Vec<&str> = if has_ignore {
            c.files_changed
                .iter()
                .filter(|f| {
                    !config
                        .ignore
                        .iter()
                        .any(|pat| crate::config::glob_match(pat, f))
                })
                .map(|f| f.as_str())
                .collect()
        } else {
            c.files_changed.iter().map(|f| f.as_str()).collect()
        };

        if relevant_files.is_empty() {
            continue;
        }

        let touches_global_dep = !config.dependencies.is_empty()
            && relevant_files.iter().any(|f| {
                config
                    .dependencies
                    .iter()
                    .any(|pat| crate::config::glob_match(pat, f))
            });

        if touches_global_dep {
            for name in &all_pkg_names {
                pkg_commit_indices.entry(name).or_default().push(i);
            }
        } else {
            // Deduplicate: a commit touching multiple files in the same package
            let mut seen = HashSet::new();
            for f in &relevant_files {
                if let Some(pkg) = file_to_package(f, packages)
                    && seen.insert(pkg.name.as_str())
                {
                    pkg_commit_indices
                        .entry(pkg.name.as_str())
                        .or_default()
                        .push(i);
                }
            }
        }
    }

    // Free file lists now that the inverted index is built.
    for c in &mut all_commits {
        c.files_changed = Vec::new();
    }

    // Build OID→index map for O(1) cutoff lookups.
    let oid_to_idx: HashMap<git2::Oid, usize> = all_commits
        .iter()
        .enumerate()
        .filter_map(|(i, c)| c.oid.map(|oid| (oid, i)))
        .collect();

    let releases: Vec<Option<PackageRelease>> = packages
        .par_iter()
        .zip(tag_infos.par_iter())
        .map(|(pkg, tag_info)| {
            // On maintenance branches, skip packages whose version is outside the range.
            if branch_ctx.maintenance
                && let Some(ref range) = branch_ctx.maintenance_range
                && !version_in_maintenance_range(&tag_info.current_version, range)
            {
                return Ok(None);
            }

            let cutoff_idx = tag_info
                .cutoff_oid
                .and_then(|cutoff| oid_to_idx.get(&cutoff).copied());
            let inclusive = tag_info.cutoff_inclusive;

            let pkg_commits: Vec<ConventionalCommit> = pkg_commit_indices
                .get(pkg.name.as_str())
                .map(|idxs| {
                    idxs.iter()
                        .filter(|&&i| match cutoff_idx {
                            Some(cut) if inclusive => i <= cut,
                            Some(cut) => i < cut,
                            None => true,
                        })
                        .map(|&i| all_commits[i].clone())
                        .collect()
                })
                .unwrap_or_default();

            // Per package: a revert whose target is already released stays
            // alone in this package's range and yields a patch.
            let pkg_commits = crate::commit::filter_reverted_commits(pkg_commits);

            if pkg_commits.is_empty() {
                return Ok(None);
            }

            let next_version =
                calculate_next_version(&tag_info.current_version, &pkg_commits, branch_ctx)?;

            if next_version == tag_info.current_version {
                return Ok(None);
            }

            // Fail if this version already exists as a tag on another branch (collision).
            if next_version.pre.is_empty() && tag_index.version_exists(&pkg.name, &next_version) {
                anyhow::bail!(
                    "Version {} for '{}' already exists as a tag. \
                     Branch '{}' cannot release this version \
                     because it was already released on another branch.",
                    next_version,
                    pkg.name,
                    branch_ctx.branch_name,
                );
            }

            let bump = classify_bump(&tag_info.current_version, &next_version);

            if bump > BumpLevel::None {
                Ok(Some(PackageRelease {
                    package_name: pkg.name.clone(),
                    current_version: tag_info.current_version.clone(),
                    next_version,
                    bump,
                    commits: pkg_commits,
                    is_root: pkg.is_root,
                    propagated_from: None,
                }))
            } else {
                Ok(None)
            }
        })
        .collect::<Result<Vec<_>>>()?;

    let mut releases: Vec<PackageRelease> = releases.into_iter().flatten().collect();

    propagate_to_dependents(&mut releases, packages, &tag_infos, &tag_index, branch_ctx)?;

    // Tripwire: a prerelease-configured branch must never plan a stable release,
    // no matter which path produced the version.
    if let Some(ref channel) = branch_ctx.prerelease {
        for r in &releases {
            if !prerelease_matches_channel(r.next_version.pre.as_str(), channel) {
                anyhow::bail!(
                    "Internal error: planned version {} for '{}' is not on prerelease channel '{}'",
                    r.next_version,
                    r.package_name,
                    channel,
                );
            }
        }
    }

    Ok(releases)
}

/// Recursively propagate releases through the reverse dependency graph.
fn propagate_to_dependents(
    releases: &mut Vec<PackageRelease>,
    packages: &[Package],
    tag_infos: &[PkgTagInfo],
    tag_index: &git::TagIndex,
    branch_ctx: &BranchContext,
) -> Result<()> {
    let mut reverse_deps: HashMap<&str, Vec<usize>> = HashMap::new();
    for (i, pkg) in packages.iter().enumerate() {
        for dep_name in pkg.local_dependencies.keys() {
            reverse_deps.entry(dep_name.as_str()).or_default().push(i);
        }
    }

    let mut released: HashSet<String> = releases.iter().map(|r| r.package_name.clone()).collect();

    // BFS queue: (package_name_that_triggered, chain_so_far)
    let mut queue: std::collections::VecDeque<(String, String)> = releases
        .iter()
        .map(|r| (r.package_name.clone(), r.package_name.clone()))
        .collect();

    while let Some((trigger_name, chain)) = queue.pop_front() {
        let Some(dependents) = reverse_deps.get(trigger_name.as_str()) else {
            continue;
        };
        for &dep_idx in dependents {
            let dep_pkg = &packages[dep_idx];
            if released.contains(&dep_pkg.name) {
                continue;
            }

            let tag_info = &tag_infos[dep_idx];

            // Like the direct-release filter in `determine_releases`, dependents
            // outside a maintenance branch's range must not receive cascaded bumps.
            if branch_ctx.maintenance
                && let Some(ref range) = branch_ctx.maintenance_range
                && !version_in_maintenance_range(&tag_info.current_version, range)
            {
                released.insert(dep_pkg.name.clone());
                eprintln!(
                    "  [version] Skipping cascade to '{}' v{}: outside maintenance range for branch '{}'",
                    dep_pkg.name, tag_info.current_version, branch_ctx.branch_name
                );
                continue;
            }

            released.insert(dep_pkg.name.clone());

            let next_version = propagated_next_version(&tag_info.current_version, branch_ctx)?;

            // Unlike a direct release, a propagated bump was never asked for by
            // any commit — on a collision with a tag from another branch, skip
            // the dependent with a warning instead of failing the whole run.
            if next_version.pre.is_empty() && tag_index.version_exists(&dep_pkg.name, &next_version)
            {
                eprintln!(
                    "  [version] Skipping cascade to '{}': version {} already exists as a tag on another branch (dependency chain: {})",
                    dep_pkg.name, next_version, chain
                );
                continue;
            }

            let next_chain = format!("{} -> {}", chain, dep_pkg.name);

            releases.push(PackageRelease {
                package_name: dep_pkg.name.clone(),
                current_version: tag_info.current_version.clone(),
                next_version,
                bump: BumpLevel::Patch,
                commits: Vec::new(),
                is_root: dep_pkg.is_root,
                propagated_from: Some(chain.clone()),
            });

            queue.push_back((dep_pkg.name.clone(), next_chain));
        }
    }

    Ok(())
}

/// Next version for a release triggered purely by a workspace dependency
/// update (no direct commits). Dependency updates are patch-level by
/// convention; on prerelease branches the result must stay on the channel —
/// a prerelease-configured branch must never produce a stable version.
fn propagated_next_version(current: &Version, branch_ctx: &BranchContext) -> Result<Version> {
    if let Some(ref channel) = branch_ctx.prerelease {
        // Already on this channel -> increment the prerelease number;
        // stable base -> x.y.(z+1)-<channel>.1
        let dep_commit = crate::commit::parse_conventional_commit(
            "00000000",
            "fix: workspace dependency update",
        )
        .expect("synthetic dependency-update commit is a valid conventional commit");
        return calculate_prerelease_version(current, &[dep_commit], channel);
    }

    // Stable and maintenance branches: patch + 1, prerelease and build metadata stripped.
    Ok(apply_bump(current, BumpLevel::Patch))
}

/// Find the oldest tag among all packages by comparing commit timestamps.
fn find_oldest_tag<'a>(repo: &Repository, tag_infos: &'a [PkgTagInfo]) -> Result<Option<&'a str>> {
    let mut oldest: Option<(&str, i64)> = None;

    for info in tag_infos {
        if let (Some(tag), Some(oid)) = (&info.cutoff_tag, info.cutoff_oid) {
            let commit = repo.find_commit(oid)?;
            let time = commit.time().seconds();
            let tag_str: &'a str = tag;
            match oldest {
                None => oldest = Some((tag_str, time)),
                Some((_, oldest_time)) if time < oldest_time => {
                    oldest = Some((tag_str, time));
                }
                _ => {}
            }
        }
    }

    Ok(oldest.map(|(tag, _)| tag))
}

fn calculate_next_version(
    current: &Version,
    commits: &[ConventionalCommit],
    branch_ctx: &BranchContext,
) -> Result<Version> {
    // chore/docs/ci/style/test/build/refactor and bare reverts don't trigger releases.
    let bump_commits: Vec<ConventionalCommit> = commits
        .iter()
        .filter(|c| c.bump > BumpLevel::None)
        .cloned()
        .collect();

    if bump_commits.is_empty() {
        return Ok(current.clone());
    }

    if let Some(ref channel) = branch_ctx.prerelease {
        return calculate_prerelease_version(current, &bump_commits, channel);
    }

    if branch_ctx.maintenance {
        return calculate_maintenance_version(
            current,
            &bump_commits,
            branch_ctx.maintenance_range.as_ref(),
        );
    }

    calculate_stable_version(current, &bump_commits)
}

fn calculate_stable_version(current: &Version, commits: &[ConventionalCommit]) -> Result<Version> {
    let cliff_release = git_cliff_core::release::Release {
        version: None,
        commits: crate::notes::to_cliff_commits(commits),
        previous: Some(Box::new(git_cliff_core::release::Release {
            version: Some(current.to_string()),
            ..Default::default()
        })),
        ..Default::default()
    };

    let next = cliff_release
        .calculate_next_version()
        .map_err(|e| anyhow::anyhow!("Failed to calculate next version: {}", e))?;

    Version::parse(&next.version).or_else(|_| Ok(apply_bump_fallback(current, commits)))
}

fn calculate_prerelease_version(
    current: &Version,
    commits: &[ConventionalCommit],
    channel: &str,
) -> Result<Version> {
    let current_channel = extract_prerelease_channel(current);

    if current_channel.as_deref() == Some(channel) {
        let next_num = extract_prerelease_number(current) + 1;
        let mut next = current.clone();
        next.pre = Prerelease::new(&format!("{}.{}", channel, next_num))
            .map_err(|e| anyhow::anyhow!("Invalid prerelease: {}", e))?;
        // A released version must not inherit build metadata from its base.
        next.build = semver::BuildMetadata::EMPTY;
        return Ok(next);
    }

    let base = Version::new(current.major, current.minor, current.patch);
    let next_stable = calculate_stable_version(&base, commits)?;

    let mut next = next_stable;
    next.pre = Prerelease::new(&format!("{}.1", channel))
        .map_err(|e| anyhow::anyhow!("Invalid prerelease: {}", e))?;
    Ok(next)
}

/// Check if a version fits within a maintenance range.
fn version_in_maintenance_range(v: &Version, range: &MaintenanceRange) -> bool {
    match range {
        MaintenanceRange::Major(maj) => v.major == *maj,
        MaintenanceRange::MajorMinor(maj, min) => v.major == *maj && v.minor == *min,
    }
}

fn calculate_maintenance_version(
    current: &Version,
    commits: &[ConventionalCommit],
    range: Option<&MaintenanceRange>,
) -> Result<Version> {
    let next = calculate_stable_version(current, commits)?;

    match range {
        // `1.5.x` — lock major AND minor, only patch bumps allowed.
        Some(crate::config::MaintenanceRange::MajorMinor(_, _)) => {
            if next.major > current.major || next.minor > current.minor {
                Ok(Version::new(
                    current.major,
                    current.minor,
                    current.patch + 1,
                ))
            } else {
                Ok(next)
            }
        }
        // `1.x` — lock major, minor bumps are allowed but major bumps are capped.
        Some(crate::config::MaintenanceRange::Major(_)) | None => {
            if next.major > current.major {
                Ok(Version::new(current.major, current.minor + 1, 0))
            } else {
                Ok(next)
            }
        }
    }
}

/// Check if a prerelease string matches a channel (e.g. "beta.1" matches "beta").
pub fn prerelease_matches_channel(pre: &str, channel: &str) -> bool {
    pre == channel || pre.starts_with(&format!("{}.", channel))
}

fn extract_prerelease_channel(version: &Version) -> Option<String> {
    let pre = version.pre.as_str();
    if pre.is_empty() {
        return None;
    }
    if let Some(dot_pos) = pre.rfind('.') {
        let after_dot = &pre[dot_pos + 1..];
        if after_dot.parse::<u64>().is_ok() {
            return Some(pre[..dot_pos].to_string());
        }
    }
    Some(pre.to_string())
}

fn extract_prerelease_number(version: &Version) -> u64 {
    let pre = version.pre.as_str();
    if let Some(dot_pos) = pre.rfind('.') {
        pre[dot_pos + 1..].parse().unwrap_or(0)
    } else {
        0
    }
}

fn classify_bump(current: &Version, next: &Version) -> BumpLevel {
    if !next.pre.is_empty() {
        if next.major > current.major
            || (current.pre.is_empty() && next.minor > current.minor)
            || (!current.pre.is_empty()
                && extract_prerelease_channel(current) != extract_prerelease_channel(next))
        {
            return BumpLevel::Minor;
        }
        return BumpLevel::Patch;
    }
    if next.major > current.major {
        BumpLevel::Major
    } else if next.minor > current.minor {
        BumpLevel::Minor
    } else if next.patch > current.patch {
        BumpLevel::Patch
    } else {
        BumpLevel::None
    }
}

fn apply_bump_fallback(version: &Version, commits: &[ConventionalCommit]) -> Version {
    let bump = commits
        .iter()
        .map(|c| c.bump)
        .max()
        .unwrap_or(BumpLevel::None);
    apply_bump(version, bump)
}

pub fn apply_bump(version: &Version, bump: BumpLevel) -> Version {
    match bump {
        BumpLevel::None => version.clone(),
        BumpLevel::Patch => Version::new(version.major, version.minor, version.patch + 1),
        BumpLevel::Minor => Version::new(version.major, version.minor + 1, 0),
        BumpLevel::Major => {
            if version.major == 0 {
                Version::new(0, version.minor + 1, 0)
            } else {
                Version::new(version.major + 1, 0, 0)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::MaintenanceRange;
    use crate::test_fixtures::make_pkg;

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

    #[test]
    fn test_extract_prerelease_channel() {
        let v = Version::parse("2.0.0-beta.3").unwrap();
        assert_eq!(extract_prerelease_channel(&v), Some("beta".into()));

        let v = Version::parse("1.0.0-rc.1").unwrap();
        assert_eq!(extract_prerelease_channel(&v), Some("rc".into()));

        let v = Version::parse("1.0.0-next.10").unwrap();
        assert_eq!(extract_prerelease_channel(&v), Some("next".into()));

        let v = Version::parse("1.0.0").unwrap();
        assert_eq!(extract_prerelease_channel(&v), None);
    }

    #[test]
    fn test_extract_prerelease_number() {
        let v = Version::parse("2.0.0-beta.3").unwrap();
        assert_eq!(extract_prerelease_number(&v), 3);

        let v = Version::parse("1.0.0-rc.15").unwrap();
        assert_eq!(extract_prerelease_number(&v), 15);

        let v = Version::parse("1.0.0").unwrap();
        assert_eq!(extract_prerelease_number(&v), 0);
    }

    #[test]
    fn test_prerelease_increment() {
        let current = Version::parse("2.0.0-beta.3").unwrap();
        let commits = vec![make_commit("fix: something")];
        let result = calculate_prerelease_version(&current, &commits, "beta").unwrap();
        assert_eq!(result, Version::parse("2.0.0-beta.4").unwrap());
    }

    #[test]
    fn test_prerelease_increment_strips_build_metadata() {
        let current = Version::parse("2.0.0-beta.3+sha.abc").unwrap();
        let commits = vec![make_commit("fix: something")];
        let result = calculate_prerelease_version(&current, &commits, "beta").unwrap();
        assert_eq!(result, Version::parse("2.0.0-beta.4").unwrap());
    }

    #[test]
    fn test_prerelease_from_stable() {
        let current = Version::parse("1.0.0").unwrap();
        let commits = vec![make_commit("feat: new thing")];
        let result = calculate_prerelease_version(&current, &commits, "beta").unwrap();
        assert_eq!(result, Version::parse("1.1.0-beta.1").unwrap());
    }

    #[test]
    fn test_maintenance_major_minor_caps_breaking_to_patch() {
        let range = Some(MaintenanceRange::MajorMinor(1, 5));
        let current = Version::parse("1.5.0").unwrap();
        let commits = vec![make_commit("feat!: breaking change")];
        let result = calculate_maintenance_version(&current, &commits, range.as_ref()).unwrap();
        assert_eq!(result, Version::parse("1.5.1").unwrap());
    }

    #[test]
    fn test_maintenance_major_minor_caps_feat_to_patch() {
        let range = Some(MaintenanceRange::MajorMinor(1, 5));
        let current = Version::parse("1.5.0").unwrap();
        let commits = vec![make_commit("feat: add feature")];
        let result = calculate_maintenance_version(&current, &commits, range.as_ref()).unwrap();
        assert_eq!(result, Version::parse("1.5.1").unwrap());
    }

    #[test]
    fn test_maintenance_major_allows_minor() {
        let range = Some(MaintenanceRange::Major(1));
        let current = Version::parse("1.5.0").unwrap();
        let commits = vec![make_commit("feat: add feature")];
        let result = calculate_maintenance_version(&current, &commits, range.as_ref()).unwrap();
        assert_eq!(result, Version::parse("1.6.0").unwrap());
    }

    #[test]
    fn test_maintenance_major_caps_breaking() {
        let range = Some(MaintenanceRange::Major(1));
        let current = Version::parse("1.5.0").unwrap();
        let commits = vec![make_commit("feat!: breaking change")];
        let result = calculate_maintenance_version(&current, &commits, range.as_ref()).unwrap();
        assert_eq!(result, Version::parse("1.6.0").unwrap());
    }

    #[test]
    fn test_maintenance_allows_patch() {
        let range = Some(MaintenanceRange::MajorMinor(1, 5));
        let current = Version::parse("1.5.2").unwrap();
        let commits = vec![make_commit("fix: bug fix")];
        let result = calculate_maintenance_version(&current, &commits, range.as_ref()).unwrap();
        assert_eq!(result, Version::parse("1.5.3").unwrap());
    }

    #[test]
    fn test_version_in_maintenance_range() {
        let v = Version::parse("1.5.0").unwrap();
        assert!(version_in_maintenance_range(
            &v,
            &MaintenanceRange::Major(1)
        ));
        assert!(!version_in_maintenance_range(
            &v,
            &MaintenanceRange::Major(2)
        ));
        assert!(version_in_maintenance_range(
            &v,
            &MaintenanceRange::MajorMinor(1, 5)
        ));
        assert!(!version_in_maintenance_range(
            &v,
            &MaintenanceRange::MajorMinor(1, 4)
        ));
    }

    fn stable_ctx() -> BranchContext {
        BranchContext {
            branch_name: "main".into(),
            prerelease: None,
            maintenance: false,
            maintenance_range: None,
            channel: None,
            packages: Vec::new(),
        }
    }

    #[test]
    fn test_chore_no_bump() {
        let v = Version::parse("1.0.0").unwrap();
        let result =
            calculate_next_version(&v, &[make_commit("chore: update deps")], &stable_ctx())
                .unwrap();
        assert_eq!(result, v, "chore should not bump");
    }

    #[test]
    fn test_docs_no_bump() {
        let v = Version::parse("1.0.0").unwrap();
        let result =
            calculate_next_version(&v, &[make_commit("docs: update readme")], &stable_ctx())
                .unwrap();
        assert_eq!(result, v, "docs should not bump");
    }

    #[test]
    fn test_ci_no_bump() {
        let v = Version::parse("1.0.0").unwrap();
        let result =
            calculate_next_version(&v, &[make_commit("ci: update workflow")], &stable_ctx())
                .unwrap();
        assert_eq!(result, v, "ci should not bump");
    }

    #[test]
    fn test_refactor_no_bump() {
        let v = Version::parse("1.0.0").unwrap();
        let result =
            calculate_next_version(&v, &[make_commit("refactor: simplify")], &stable_ctx())
                .unwrap();
        assert_eq!(result, v, "refactor should not bump");
    }

    #[test]
    fn test_style_no_bump() {
        let v = Version::parse("1.0.0").unwrap();
        let result =
            calculate_next_version(&v, &[make_commit("style: format")], &stable_ctx()).unwrap();
        assert_eq!(result, v, "style should not bump");
    }

    #[test]
    fn test_test_no_bump() {
        let v = Version::parse("1.0.0").unwrap();
        let result =
            calculate_next_version(&v, &[make_commit("test: add tests")], &stable_ctx()).unwrap();
        assert_eq!(result, v, "test should not bump");
    }

    #[test]
    fn test_build_no_bump() {
        let v = Version::parse("1.0.0").unwrap();
        let result =
            calculate_next_version(&v, &[make_commit("build: update config")], &stable_ctx())
                .unwrap();
        assert_eq!(result, v, "build should not bump");
    }

    #[test]
    fn test_feat_bumps_minor() {
        let v = Version::parse("1.0.0").unwrap();
        let result = calculate_stable_version(&v, &[make_commit("feat: add feature")]).unwrap();
        assert_eq!(result, Version::parse("1.1.0").unwrap());
    }

    #[test]
    fn test_fix_bumps_patch() {
        let v = Version::parse("1.0.0").unwrap();
        let result = calculate_stable_version(&v, &[make_commit("fix: bug fix")]).unwrap();
        assert_eq!(result, Version::parse("1.0.1").unwrap());
    }

    #[test]
    fn test_perf_bumps_patch() {
        let v = Version::parse("1.0.0").unwrap();
        let result = calculate_stable_version(&v, &[make_commit("perf: optimize")]).unwrap();
        assert_eq!(result, Version::parse("1.0.1").unwrap());
    }

    #[test]
    fn test_breaking_bumps_major() {
        let v = Version::parse("1.0.0").unwrap();
        let result = calculate_stable_version(&v, &[make_commit("feat!: redesign api")]).unwrap();
        assert_eq!(result, Version::parse("2.0.0").unwrap());
    }

    #[test]
    fn test_breaking_footer_bumps_major() {
        let v = Version::parse("1.0.0").unwrap();
        let result = calculate_stable_version(
            &v,
            &[make_commit("fix: change\n\nBREAKING CHANGE: new api")],
        )
        .unwrap();
        assert_eq!(result, Version::parse("2.0.0").unwrap());
    }

    #[test]
    fn test_highest_bump_wins() {
        let v = Version::parse("1.0.0").unwrap();
        let commits = vec![
            make_commit("fix: small fix"),
            make_commit("feat: new feature"),
            make_commit("chore: update deps"),
        ];
        let bump_commits: Vec<_> = commits
            .into_iter()
            .filter(|c| c.bump > BumpLevel::None)
            .collect();
        let result = calculate_stable_version(&v, &bump_commits).unwrap();
        assert_eq!(result, Version::parse("1.1.0").unwrap());
    }

    #[test]
    fn test_breaking_wins_over_feat() {
        let v = Version::parse("1.0.0").unwrap();
        let commits = vec![
            make_commit("feat: add feature"),
            make_commit("fix!: breaking fix"),
        ];
        let result = calculate_stable_version(&v, &commits).unwrap();
        assert_eq!(result, Version::parse("2.0.0").unwrap());
    }

    #[test]
    fn test_prerelease_feat_from_stable() {
        let v = Version::parse("1.0.0").unwrap();
        let result =
            calculate_prerelease_version(&v, &[make_commit("feat: thing")], "beta").unwrap();
        assert_eq!(result, Version::parse("1.1.0-beta.1").unwrap());
    }

    #[test]
    fn test_prerelease_fix_from_stable() {
        let v = Version::parse("1.0.0").unwrap();
        let result =
            calculate_prerelease_version(&v, &[make_commit("fix: thing")], "beta").unwrap();
        assert_eq!(result, Version::parse("1.0.1-beta.1").unwrap());
    }

    #[test]
    fn test_prerelease_breaking_from_stable() {
        let v = Version::parse("1.0.0").unwrap();
        let result =
            calculate_prerelease_version(&v, &[make_commit("feat!: break")], "beta").unwrap();
        assert_eq!(result, Version::parse("2.0.0-beta.1").unwrap());
    }

    #[test]
    fn test_maintenance_fix_bumps_patch() {
        let range = Some(MaintenanceRange::Major(1));
        let v = Version::parse("1.5.0").unwrap();
        let result =
            calculate_maintenance_version(&v, &[make_commit("fix: thing")], range.as_ref())
                .unwrap();
        assert_eq!(result, Version::parse("1.5.1").unwrap());
    }

    #[test]
    fn test_maintenance_major_range_feat_bumps_minor() {
        let range = Some(MaintenanceRange::Major(1));
        let v = Version::parse("1.5.0").unwrap();
        let result =
            calculate_maintenance_version(&v, &[make_commit("feat: thing")], range.as_ref())
                .unwrap();
        assert_eq!(result, Version::parse("1.6.0").unwrap());
    }

    #[test]
    fn test_maintenance_major_range_breaking_capped_to_minor() {
        let range = Some(MaintenanceRange::Major(1));
        let v = Version::parse("1.5.0").unwrap();
        let result =
            calculate_maintenance_version(&v, &[make_commit("feat!: break")], range.as_ref())
                .unwrap();
        assert_eq!(result.major, 1, "Major should stay capped at 1");
        assert_eq!(result, Version::parse("1.6.0").unwrap());
    }

    const REVERT_SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    /// Exercises the real git-cliff path with the normalized message.
    #[test]
    fn test_lone_git_style_revert_bumps_patch() {
        let v = Version::parse("1.1.0").unwrap();
        let msg = format!("Revert \"feat: add login\"\n\nThis reverts commit {REVERT_SHA}.");
        let result = calculate_next_version(&v, &[make_commit(&msg)], &stable_ctx()).unwrap();
        assert_eq!(result, Version::parse("1.1.1").unwrap());
    }

    #[test]
    fn test_lone_angular_revert_bumps_patch() {
        let v = Version::parse("1.1.0").unwrap();
        let msg = format!("revert: feat: add login\n\nThis reverts commit {REVERT_SHA}.");
        let result = calculate_next_version(&v, &[make_commit(&msg)], &stable_ctx()).unwrap();
        assert_eq!(result, Version::parse("1.1.1").unwrap());
    }

    /// semantic-release parity: `revert:` without the footer is not a release.
    #[test]
    fn test_bare_revert_no_release() {
        let v = Version::parse("1.0.0").unwrap();
        let result =
            calculate_next_version(&v, &[make_commit("revert: undo thing")], &stable_ctx())
                .unwrap();
        assert_eq!(result, v, "bare revert should not bump");
    }

    #[test]
    fn test_lone_revert_prerelease_bumps_patch() {
        let v = Version::parse("1.0.0").unwrap();
        let msg = format!("Revert \"feat: add login\"\n\nThis reverts commit {REVERT_SHA}.");
        let result = calculate_prerelease_version(&v, &[make_commit(&msg)], "beta").unwrap();
        assert_eq!(result, Version::parse("1.0.1-beta.1").unwrap());
    }

    fn make_commit(message: &str) -> ConventionalCommit {
        crate::commit::parse_conventional_commit("abcd1234", message).unwrap()
    }

    fn prerelease_ctx(channel: &str) -> BranchContext {
        BranchContext {
            branch_name: channel.into(),
            prerelease: Some(channel.into()),
            channel: Some(channel.into()),
            ..stable_ctx()
        }
    }

    fn make_tag_info(version: &str) -> PkgTagInfo {
        PkgTagInfo {
            current_version: Version::parse(version).unwrap(),
            cutoff_oid: None,
            cutoff_tag: None,
            cutoff_inclusive: false,
        }
    }

    /// Seed for the propagation BFS; only the package name matters there.
    fn seed_release(name: &str) -> PackageRelease {
        PackageRelease {
            package_name: name.into(),
            current_version: Version::new(1, 0, 0),
            next_version: Version::new(1, 0, 1),
            bump: BumpLevel::Patch,
            commits: Vec::new(),
            is_root: false,
            propagated_from: None,
        }
    }

    fn empty_tag_index() -> git::TagIndex {
        git::TagIndex::from_stable_versions(HashMap::new())
    }

    #[test]
    fn test_propagate_prerelease_dependent_on_channel_increments() {
        let packages = vec![
            make_pkg("@test/assets", &[]),
            make_pkg("@test/components", &["@test/assets"]),
        ];
        let tag_infos = vec![
            make_tag_info("1.19.0-test-tasks-tsmain-2.1"),
            make_tag_info("10.267.0-test-tasks-tsmain-2.1"),
        ];
        let mut releases = vec![seed_release("@test/assets")];
        let ctx = prerelease_ctx("test-tasks-tsmain-2");

        propagate_to_dependents(
            &mut releases,
            &packages,
            &tag_infos,
            &empty_tag_index(),
            &ctx,
        )
        .unwrap();

        let dep = releases
            .iter()
            .find(|r| r.package_name == "@test/components")
            .expect("dependent should be released via propagation");
        assert_eq!(
            dep.next_version,
            Version::parse("10.267.0-test-tasks-tsmain-2.2").unwrap()
        );
        assert_eq!(dep.bump, BumpLevel::Patch);
        assert_eq!(dep.propagated_from.as_deref(), Some("@test/assets"));
    }

    #[test]
    fn test_propagate_prerelease_dependent_from_stable_base() {
        let packages = vec![
            make_pkg("@test/core", &[]),
            make_pkg("@test/app", &["@test/core"]),
        ];
        let tag_infos = vec![make_tag_info("1.0.0-beta.1"), make_tag_info("10.267.0")];
        let mut releases = vec![seed_release("@test/core")];
        let ctx = prerelease_ctx("beta");

        propagate_to_dependents(
            &mut releases,
            &packages,
            &tag_infos,
            &empty_tag_index(),
            &ctx,
        )
        .unwrap();

        let dep = releases
            .iter()
            .find(|r| r.package_name == "@test/app")
            .expect("dependent should be released via propagation");
        assert_eq!(dep.next_version, Version::parse("10.267.1-beta.1").unwrap());
    }

    #[test]
    fn test_propagate_prerelease_transitive_chain_never_stable() {
        let packages = vec![
            make_pkg("a", &[]),
            make_pkg("b", &["a"]),
            make_pkg("c", &["b"]),
        ];
        let tag_infos = vec![
            make_tag_info("1.0.0-beta.1"),
            make_tag_info("1.1.0-beta.2"),
            make_tag_info("2.0.0"),
        ];
        let mut releases = vec![seed_release("a")];
        let ctx = prerelease_ctx("beta");

        propagate_to_dependents(
            &mut releases,
            &packages,
            &tag_infos,
            &empty_tag_index(),
            &ctx,
        )
        .unwrap();

        let b = releases.iter().find(|r| r.package_name == "b").unwrap();
        assert_eq!(b.next_version, Version::parse("1.1.0-beta.3").unwrap());
        let c = releases.iter().find(|r| r.package_name == "c").unwrap();
        assert_eq!(c.next_version, Version::parse("2.0.1-beta.1").unwrap());
        assert_eq!(c.propagated_from.as_deref(), Some("a -> b"));
    }

    #[test]
    fn test_propagate_stable_branch_unchanged() {
        let packages = vec![
            make_pkg("@test/core", &[]),
            make_pkg("@test/app", &["@test/core"]),
        ];
        let tag_infos = vec![make_tag_info("1.0.0"), make_tag_info("1.2.3+build.5")];
        let mut releases = vec![seed_release("@test/core")];

        propagate_to_dependents(
            &mut releases,
            &packages,
            &tag_infos,
            &empty_tag_index(),
            &stable_ctx(),
        )
        .unwrap();

        let dep = releases
            .iter()
            .find(|r| r.package_name == "@test/app")
            .unwrap();
        assert_eq!(dep.next_version, Version::parse("1.2.4").unwrap());
        assert_eq!(dep.bump, BumpLevel::Patch);
    }

    #[test]
    fn test_propagate_collision_skips_dependent_and_its_cascade() {
        let packages = vec![
            make_pkg("@test/core", &[]),
            make_pkg("@test/app", &["@test/core"]),
            make_pkg("@test/ui", &["@test/app"]),
        ];
        let tag_infos = vec![
            make_tag_info("1.0.0"),
            make_tag_info("1.0.0"),
            make_tag_info("1.0.0"),
        ];
        let mut releases = vec![seed_release("@test/core")];

        let mut stable: HashMap<String, HashSet<Version>> = HashMap::new();
        stable.insert("@test/app".into(), HashSet::from([Version::new(1, 0, 1)]));
        let tag_index = git::TagIndex::from_stable_versions(stable);

        propagate_to_dependents(
            &mut releases,
            &packages,
            &tag_infos,
            &tag_index,
            &stable_ctx(),
        )
        .unwrap();

        // The colliding dependent is skipped, and nothing cascades past it.
        assert_eq!(releases.len(), 1);
        assert!(releases.iter().all(|r| r.package_name == "@test/core"));
    }

    #[test]
    fn test_propagate_prerelease_wins_over_maintenance() {
        let packages = vec![
            make_pkg("@test/core", &[]),
            make_pkg("@test/app", &["@test/core"]),
        ];
        let tag_infos = vec![make_tag_info("1.0.0"), make_tag_info("1.5.0")];
        let mut releases = vec![seed_release("@test/core")];
        let mut ctx = prerelease_ctx("beta");
        ctx.maintenance = true;
        ctx.maintenance_range = Some(MaintenanceRange::Major(1));

        propagate_to_dependents(
            &mut releases,
            &packages,
            &tag_infos,
            &empty_tag_index(),
            &ctx,
        )
        .unwrap();

        let dep = releases
            .iter()
            .find(|r| r.package_name == "@test/app")
            .unwrap();
        assert_eq!(dep.next_version, Version::parse("1.5.1-beta.1").unwrap());
    }
}
