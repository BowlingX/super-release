use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::Result;
use git2::Repository;
use rayon::prelude::*;
use semver::{Prerelease, Version};

use crate::commit::{BumpLevel, ConventionalCommit};
use crate::config::{BranchContext, Config};
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
}

struct PkgTagInfo {
    current_version: Version,
    cutoff_oid: Option<git2::Oid>,
    cutoff_tag: Option<String>,
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
    // 1. Build tag index once (single revwalk + single tag enumeration).
    let pkg_pairs: Vec<(String, bool)> = packages
        .iter()
        .map(|p| (p.name.clone(), p.is_root))
        .collect();
    let tag_index = git::TagIndex::build(repo, &pkg_pairs, config, branch_ctx)?;

    let tag_infos: Vec<PkgTagInfo> = packages
        .iter()
        .map(|pkg| match tag_index.latest_version(&pkg.name) {
            Some((tag_name, ver)) => {
                let oid = git::tag_to_oid(repo, &tag_name)?;
                Ok(PkgTagInfo {
                    current_version: ver,
                    cutoff_oid: oid,
                    cutoff_tag: Some(tag_name),
                })
            }
            None => Ok(PkgTagInfo {
                current_version: pkg.version.clone(),
                cutoff_oid: None,
                cutoff_tag: None,
            }),
        })
        .collect::<Result<Vec<_>>>()?;

    // 2. Find the oldest tag across all packages — we only need commits since that point.
    //    If any package has no tag (first release), we must walk the full history.
    let all_have_tags = tag_infos.iter().all(|t| t.cutoff_tag.is_some());
    let oldest_tag: Option<&str> = if all_have_tags {
        find_oldest_tag(repo, &tag_infos)?
    } else {
        None
    };

    // 3. Walk commits only from HEAD to the oldest tag boundary.
    let all_commits = git::get_commits_since(repo, repo_path, oldest_tag)?;

    // 4. Precompute file→package name mapping once.
    //    If any file in a commit matches a global dependency pattern,
    //    that commit affects ALL packages.
    // 4. Build inverted index: package_name → Vec<commit_index> in a single pass.
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

    // 5. Process each package in parallel using the inverted index.
    let releases: Vec<Option<PackageRelease>> = packages
        .par_iter()
        .zip(tag_infos.par_iter())
        .map(|(pkg, tag_info)| {
            // Find the cutoff index: the position in all_commits of the tagged commit.
            // All commits before this index (lower index = newer) are since the tag.
            let cutoff_idx = tag_info
                .cutoff_oid
                .and_then(|cutoff| all_commits.iter().position(|c| c.oid == Some(cutoff)));

            let pkg_commits: Vec<ConventionalCommit> = pkg_commit_indices
                .get(pkg.name.as_str())
                .map(|idxs| {
                    idxs.iter()
                        .filter(|&&i| match cutoff_idx {
                            Some(cut) => i < cut,
                            None => true,
                        })
                        .map(|&i| all_commits[i].clone())
                        .collect()
                })
                .unwrap_or_default();

            if pkg_commits.is_empty() {
                return Ok(None);
            }

            let next_version =
                calculate_next_version(&tag_info.current_version, &pkg_commits, branch_ctx)?;

            if next_version == tag_info.current_version {
                return Ok(None);
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
                }))
            } else {
                Ok(None)
            }
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(releases.into_iter().flatten().collect())
}

/// Find the oldest tag among all packages by comparing commit timestamps.
/// Returns the tag name that should be used as the walk boundary.
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
    // Filter once: only bump-worthy commits feed into version calculation.
    // chore/docs/ci/style/test/build/refactor don't trigger releases.
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
        return calculate_maintenance_version(current, &bump_commits);
    }

    calculate_stable_version(current, &bump_commits)
}

fn calculate_stable_version(current: &Version, commits: &[ConventionalCommit]) -> Result<Version> {
    let cliff_release = git_cliff_core::release::Release {
        version: None,
        commits: crate::plugin::changelog::to_cliff_commits(commits),
        previous: Some(Box::new(git_cliff_core::release::Release {
            version: Some(current.to_string()),
            ..Default::default()
        })),
        ..Default::default()
    };

    let next_str = cliff_release
        .calculate_next_version()
        .map_err(|e| anyhow::anyhow!("Failed to calculate next version: {}", e))?;

    Version::parse(&next_str).or_else(|_| Ok(apply_bump_fallback(current, commits)))
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
        return Ok(next);
    }

    let base = Version::new(current.major, current.minor, current.patch);
    let next_stable = calculate_stable_version(&base, commits)?;

    let mut next = next_stable;
    next.pre = Prerelease::new(&format!("{}.1", channel))
        .map_err(|e| anyhow::anyhow!("Invalid prerelease: {}", e))?;
    Ok(next)
}

fn calculate_maintenance_version(
    current: &Version,
    commits: &[ConventionalCommit],
) -> Result<Version> {
    let next = calculate_stable_version(current, commits)?;

    if next.major > current.major {
        Ok(Version::new(current.major, current.minor + 1, 0))
    } else {
        Ok(next)
    }
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
    fn test_prerelease_from_stable() {
        let current = Version::parse("1.0.0").unwrap();
        let commits = vec![make_commit("feat: new thing")];
        let result = calculate_prerelease_version(&current, &commits, "beta").unwrap();
        assert_eq!(result, Version::parse("1.1.0-beta.1").unwrap());
    }

    #[test]
    fn test_maintenance_caps_major() {
        let current = Version::parse("1.5.0").unwrap();
        let commits = vec![make_commit("feat!: breaking change")];
        let result = calculate_maintenance_version(&current, &commits).unwrap();
        assert_eq!(result.major, 1);
        assert_eq!(result, Version::parse("1.6.0").unwrap());
    }

    #[test]
    fn test_maintenance_allows_minor() {
        let current = Version::parse("1.5.0").unwrap();
        let commits = vec![make_commit("feat: add feature")];
        let result = calculate_maintenance_version(&current, &commits).unwrap();
        assert_eq!(result, Version::parse("1.6.0").unwrap());
    }

    #[test]
    fn test_maintenance_allows_patch() {
        let current = Version::parse("1.5.2").unwrap();
        let commits = vec![make_commit("fix: bug fix")];
        let result = calculate_maintenance_version(&current, &commits).unwrap();
        assert_eq!(result, Version::parse("1.5.3").unwrap());
    }

    // ── no-bump commit types should not trigger releases ──

    fn stable_ctx() -> BranchContext {
        BranchContext {
            branch_name: "main".into(),
            prerelease: None,
            maintenance: false,
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
        // Only fix + feat passed (chore filtered out before calling this).
        // feat (minor) wins over fix (patch).
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

    // ── prerelease version calculation per commit type ──

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

    // ── maintenance version calculation per commit type ──

    #[test]
    fn test_maintenance_fix_bumps_patch() {
        let v = Version::parse("1.5.0").unwrap();
        let result = calculate_maintenance_version(&v, &[make_commit("fix: thing")]).unwrap();
        assert_eq!(result, Version::parse("1.5.1").unwrap());
    }

    #[test]
    fn test_maintenance_feat_bumps_minor() {
        let v = Version::parse("1.5.0").unwrap();
        let result = calculate_maintenance_version(&v, &[make_commit("feat: thing")]).unwrap();
        assert_eq!(result, Version::parse("1.6.0").unwrap());
    }

    #[test]
    fn test_maintenance_breaking_capped_to_minor() {
        let v = Version::parse("1.5.0").unwrap();
        let result = calculate_maintenance_version(&v, &[make_commit("feat!: break")]).unwrap();
        assert_eq!(result.major, 1, "Major should stay capped at 1");
        assert_eq!(result, Version::parse("1.6.0").unwrap());
    }

    fn make_commit(message: &str) -> ConventionalCommit {
        crate::commit::parse_conventional_commit("abcd1234", message).unwrap()
    }
}
