use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::LazyLock;

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
    pub propagated_from: Option<Vec<String>>,
}

struct PkgTagInfo {
    /// The version to use as the base for calculating the next version.
    current_version: Version,
    /// The commit whose history is already released — may differ from the tag
    /// that produced `current_version` on prerelease branches.
    cutoff_oid: Option<git2::Oid>,
    cutoff_tag: Option<String>,
    /// When true, the cutoff is the commit that introduced the package (a first
    /// release, no tag yet): that commit and what followed it are unreleased.
    cutoff_inclusive: bool,
}

/// What HEAD is while planning releases.
#[derive(Debug, Clone, Copy)]
pub enum Head {
    /// The tip of the release branch.
    BranchTip,
    /// A pull-request head previewed against the release branch. That branch's
    /// tip, when it resolves, carries releases HEAD may not contain yet.
    Preview { release_tip: Option<git2::Oid> },
}

impl Head {
    fn release_tip(self) -> Option<git2::Oid> {
        match self {
            Head::BranchTip => None,
            Head::Preview { release_tip } => release_tip,
        }
    }
}

/// Determine the next version for all packages based on commits since their last release.
///
/// Resolves tags first, then only walks the commits not yet released for
/// every package — avoids parsing the entire git history.
pub fn determine_releases(
    repo: &Repository,
    repo_path: &Path,
    packages: &[Package],
    config: &Config,
    branch_ctx: &BranchContext,
    head: Head,
) -> Result<Vec<PackageRelease>> {
    let pkg_pairs: Vec<(String, bool)> = packages
        .iter()
        .map(|p| (p.name.clone(), p.is_root))
        .collect();
    let release_tip = head.release_tip();
    let tag_index = git::TagIndex::build(repo, &pkg_pairs, config, branch_ctx, release_tip)?;

    let tag_infos: Vec<PkgTagInfo> = packages
        .iter()
        .map(|pkg| resolve_tag_info(repo, repo_path, pkg, &tag_index, branch_ctx))
        .collect::<Result<Vec<_>>>()?;

    let mut walk = git::get_commits_since(
        repo,
        repo_path,
        walk_boundary(repo, &tag_infos),
        release_tip,
    )?;
    let pkg_commit_indices = commits_by_package(&walk.commits, packages, config);

    // Free file lists now that the inverted index is built.
    for c in &mut walk.commits {
        c.files_changed = Vec::new();
    }

    let head_oid = repo.head().ok().and_then(|h| h.target());
    let scope = ReleaseScope::new(
        &walk,
        packages,
        &tag_infos,
        &pkg_commit_indices,
        release_tip.and(head_oid),
    );

    let releases: Vec<Option<PackageRelease>> = packages
        .par_iter()
        .zip(tag_infos.par_iter())
        .map(|(pkg, tag_info)| {
            let pkg_commits: Vec<ConventionalCommit> = pkg_commit_indices
                .get(pkg.name.as_str())
                .map(|idxs| {
                    idxs.iter()
                        .filter(|&&i| scope.is_unreleased(&walk, i, tag_info))
                        .map(|&i| walk.commits[i].clone())
                        .collect()
                })
                .unwrap_or_default();
            plan_release(pkg, tag_info, pkg_commits, &tag_index, branch_ctx, head)
        })
        .collect::<Result<Vec<_>>>()?;

    let mut releases: Vec<PackageRelease> = releases.into_iter().flatten().collect();

    propagate_to_dependents(&mut releases, packages, &tag_infos, &tag_index, branch_ctx)?;
    check_prerelease_plan(&releases, &tag_index, branch_ctx, head)?;

    Ok(releases)
}

/// The version a package is at and the commit that bounds what it has released.
fn resolve_tag_info(
    repo: &Repository,
    repo_path: &Path,
    pkg: &Package,
    tag_index: &git::TagIndex,
    branch_ctx: &BranchContext,
) -> Result<PkgTagInfo> {
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
            let intro_oid = git::find_file_introduction_oid(repo, repo_path, &pkg.manifest_path);
            // On stable/maintenance branches a prerelease-flavored manifest
            // version (e.g. 1.0.0-rc.1) must not leak into stable version
            // math — it would ship a prerelease under the release dist-tag.
            let current_version = if branch_ctx.prerelease.is_none() && !pkg.version.pre.is_empty()
            {
                stable_base(&pkg.version)
            } else {
                pkg.version.clone()
            };
            Ok(PkgTagInfo {
                current_version,
                cutoff_oid: intro_oid,
                cutoff_tag: None,
                cutoff_inclusive: true,
            })
        }
    }
}

/// A commit behind every package's tag, to walk no further than it; `None`
/// (the full history) when any package has no tag yet.
fn walk_boundary(repo: &Repository, tag_infos: &[PkgTagInfo]) -> Option<git2::Oid> {
    if tag_infos.iter().any(|t| t.cutoff_tag.is_none()) {
        return None;
    }
    let mut cutoffs: Vec<git2::Oid> = tag_infos.iter().filter_map(|t| t.cutoff_oid).collect();
    cutoffs.sort_unstable();
    cutoffs.dedup();
    git::common_ancestor(repo, &cutoffs)
}

/// Per package, the indices of the commits that touch it. A commit touching a
/// file that matches a global dependency pattern affects ALL packages.
fn commits_by_package<'a>(
    commits: &[ConventionalCommit],
    packages: &'a [Package],
    config: &Config,
) -> HashMap<&'a str, Vec<usize>> {
    let ignored = |f: &str| {
        config
            .ignore
            .iter()
            .any(|pat| crate::config::glob_match(pat, f))
    };
    let global_dep = |f: &str| {
        config
            .dependencies
            .iter()
            .any(|pat| crate::config::glob_match(pat, f))
    };
    let mut indices: HashMap<&str, Vec<usize>> = HashMap::new();

    for (i, c) in commits.iter().enumerate() {
        let relevant_files: Vec<&str> = c
            .files_changed
            .iter()
            .map(String::as_str)
            .filter(|f| !ignored(f))
            .collect();

        if relevant_files.iter().any(|f| global_dep(f)) {
            for pkg in packages {
                indices.entry(pkg.name.as_str()).or_default().push(i);
            }
        } else {
            // Deduplicate: a commit touching multiple files in the same package
            let mut seen = HashSet::new();
            for f in &relevant_files {
                if let Some(pkg) = file_to_package(f, packages)
                    && seen.insert(pkg.name.as_str())
                {
                    indices.entry(pkg.name.as_str()).or_default().push(i);
                }
            }
        }
    }
    indices
}

/// Which commits of a walk each package has yet to release.
struct ReleaseScope {
    /// HEAD's history, when the walk also covers the release branch's tip.
    head: Option<git::CommitSet>,
    /// Per tag cutoff, what it released: the tagged commit and its history.
    released: HashMap<git2::Oid, git::CommitSet>,
    /// Per first-release cutoff, the commit that introduced the package and
    /// what followed it.
    introduced: HashMap<git2::Oid, git::CommitSet>,
}

impl ReleaseScope {
    /// Packages released together share a cutoff: one set per distinct one,
    /// and none for packages without commits.
    fn new(
        walk: &git::CommitWalk,
        packages: &[Package],
        tag_infos: &[PkgTagInfo],
        pkg_commit_indices: &HashMap<&str, Vec<usize>>,
        preview_head: Option<git2::Oid>,
    ) -> Self {
        let cutoffs = |inclusive: bool| -> HashSet<git2::Oid> {
            packages
                .iter()
                .zip(tag_infos)
                .filter(|(pkg, t)| {
                    t.cutoff_inclusive == inclusive
                        && pkg_commit_indices.contains_key(pkg.name.as_str())
                })
                .filter_map(|(_, t)| t.cutoff_oid)
                .collect()
        };
        ReleaseScope {
            head: preview_head
                .map(|oid| walk.reachable_from(oid).unwrap_or_else(|| walk.empty_set())),
            released: cutoffs(false)
                .into_par_iter()
                .filter_map(|oid| Some((oid, walk.reachable_from(oid)?)))
                .collect(),
            introduced: cutoffs(true)
                .into_par_iter()
                .filter_map(|oid| Some((oid, walk.descended_from(oid)?)))
                .collect(),
        }
    }

    /// A cutoff the walk did not read is behind its boundary: it bounds nothing.
    fn is_unreleased(&self, walk: &git::CommitWalk, index: usize, tag_info: &PkgTagInfo) -> bool {
        if self.head.as_ref().is_some_and(|h| !walk.contains(h, index)) {
            return false;
        }
        let Some(cutoff) = tag_info.cutoff_oid else {
            return true;
        };
        if tag_info.cutoff_inclusive {
            self.introduced
                .get(&cutoff)
                .is_none_or(|set| walk.contains(set, index))
        } else {
            self.released
                .get(&cutoff)
                .is_none_or(|set| !walk.contains(set, index))
        }
    }
}

/// The release of one package from its unreleased commits, if they call for one.
fn plan_release(
    pkg: &Package,
    tag_info: &PkgTagInfo,
    pkg_commits: Vec<ConventionalCommit>,
    tag_index: &git::TagIndex,
    branch_ctx: &BranchContext,
    head: Head,
) -> Result<Option<PackageRelease>> {
    // On maintenance branches, skip packages whose version is outside the range.
    if branch_ctx.maintenance
        && let Some(ref range) = branch_ctx.maintenance_range
        && !version_in_maintenance_range(&tag_info.current_version, range)
    {
        return Ok(None);
    }

    // Per package: a revert whose target is already released stays
    // alone in this package's range and yields a patch.
    let pkg_commits = crate::commit::filter_reverted_commits(pkg_commits);

    if pkg_commits.is_empty() {
        return Ok(None);
    }

    let next_version = calculate_next_version(&tag_info.current_version, &pkg_commits, branch_ctx)?;

    if next_version == tag_info.current_version {
        return Ok(None);
    }

    // This version already exists as a tag on another branch (collision). A
    // preview's checkout may just be behind that release, so it only warns.
    if next_version.pre.is_empty() && tag_index.version_exists(&pkg.name, &next_version) {
        match head {
            Head::BranchTip => anyhow::bail!(
                "Version {} for '{}' already exists as a tag. \
                 Branch '{}' cannot release this version \
                 because it was already released on another branch.",
                next_version,
                pkg.name,
                branch_ctx.branch_name,
            ),
            Head::Preview { .. } => warn_checkout_behind(&pkg.name, &next_version, branch_ctx),
        }
    }

    let bump = classify_bump(
        &tag_info.current_version,
        &next_version,
        branch_ctx.prerelease.as_deref(),
    );

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
}

fn warn_checkout_behind(package_name: &str, version: &Version, branch_ctx: &BranchContext) {
    eprintln!(
        "  [version] Warning: version {} for '{}' already exists as a tag not \
         reachable from HEAD — the checkout may be behind the latest '{}' release",
        version, package_name, branch_ctx.branch_name
    );
}

/// Tripwire: a prerelease-configured branch must never plan a stable release,
/// no matter which path produced the version. Releases the branch's package
/// allowlist will drop are exempt — they are never published.
fn check_prerelease_plan(
    releases: &[PackageRelease],
    tag_index: &git::TagIndex,
    branch_ctx: &BranchContext,
    head: Head,
) -> Result<()> {
    let Some(ref channel) = branch_ctx.prerelease else {
        return Ok(());
    };
    for r in releases
        .iter()
        .filter(|r| branch_ctx.includes_package(&r.package_name))
    {
        if !prerelease_matches_channel(r.next_version.pre.as_str(), channel) {
            anyhow::bail!(
                "Internal error: planned version {} for '{}' is not on prerelease channel '{}'",
                r.next_version,
                r.package_name,
                channel,
            );
        }
        // A planned version can only equal an existing tag when that tag is
        // not reachable from HEAD (otherwise it would have been the base and
        // the plan would sit above it) — the branch has stale channel tags.
        // That reasoning assumes HEAD is the branch tip; previews check out
        // PR heads instead, so they warn rather than fail.
        if tag_index.version_exists(&r.package_name, &r.next_version) {
            match head {
                Head::BranchTip => anyhow::bail!(
                    "Version {} for '{}' already exists as a tag but is not reachable \
                     from branch '{}'. The branch was likely rebased or force-pushed — \
                     fetch tags and fast-forward, or delete the stale tag.",
                    r.next_version,
                    r.package_name,
                    branch_ctx.branch_name,
                ),
                Head::Preview { .. } => {
                    warn_checkout_behind(&r.package_name, &r.next_version, branch_ctx);
                }
            }
        }
    }
    Ok(())
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
    let mut queue: std::collections::VecDeque<(String, Vec<String>)> = releases
        .iter()
        .map(|r| (r.package_name.clone(), vec![r.package_name.clone()]))
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
                    dep_pkg.name,
                    next_version,
                    chain.join(" -> ")
                );
                continue;
            }

            let mut next_chain = chain.clone();
            next_chain.push(dep_pkg.name.clone());

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

/// Synthetic commit standing in for a workspace dependency update when
/// classifying the bump of a propagated release; never stored on the release.
static DEP_UPDATE_COMMIT: LazyLock<ConventionalCommit> = LazyLock::new(|| {
    crate::commit::parse_conventional_commit("00000000", "fix: workspace dependency update")
        .expect("synthetic dependency-update commit is a valid conventional commit")
});

/// Next version for a release triggered purely by a workspace dependency
/// update (no direct commits). Dependency updates are patch-level by
/// convention; on prerelease branches the result must stay on the channel —
/// a prerelease-configured branch must never produce a stable version.
fn propagated_next_version(current: &Version, branch_ctx: &BranchContext) -> Result<Version> {
    if let Some(ref channel) = branch_ctx.prerelease {
        // Already on this channel -> increment the prerelease number;
        // stable base -> x.y.(z+1)-<channel>.1
        return calculate_prerelease_version(
            current,
            std::slice::from_ref(&DEP_UPDATE_COMMIT),
            channel,
        );
    }

    // Stable and maintenance branches: patch + 1, prerelease and build metadata stripped.
    Ok(apply_bump(current, BumpLevel::Patch))
}

/// The stable part of a version — prerelease and build metadata stripped.
fn stable_base(v: &Version) -> Version {
    Version::new(v.major, v.minor, v.patch)
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

    let mut next = if let Some(ref channel) = branch_ctx.prerelease {
        calculate_prerelease_version(current, &bump_commits, channel)?
    } else if branch_ctx.maintenance {
        calculate_maintenance_version(
            current,
            &bump_commits,
            branch_ctx.maintenance_range.as_ref(),
        )?
    } else {
        calculate_stable_version(current, &bump_commits)?
    };

    // A released version never carries build metadata, whatever the base had.
    next.build = semver::BuildMetadata::EMPTY;
    Ok(next)
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
    // Parse the counter against the channel directly instead of guessing the
    // channel from the prerelease string — dotted channel names (e.g. branch
    // "test-1.0") would otherwise be split at the wrong dot. A counter at
    // u64::MAX (hand-crafted tag) falls through to the fresh-base arm instead
    // of wrapping below the base.
    if let Some(counter) = channel_counter(current.pre.as_str(), channel)
        && let Some(next_counter) = counter.checked_add(1)
    {
        let mut next = current.clone();
        next.pre = Prerelease::new(&format!("{}.{}", channel, next_counter))
            .map_err(|e| anyhow::anyhow!("Invalid prerelease: {}", e))?;
        // A released version must not inherit build metadata from its base.
        next.build = semver::BuildMetadata::EMPTY;
        return Ok(next);
    }

    let base = stable_base(current);
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

/// Counter of a `<channel>.<n>` prerelease on the given channel. `Some(0)` for
/// a counter-less prerelease equal to the channel; `None` when the suffix
/// after the channel is not numeric.
fn channel_counter(pre: &str, channel: &str) -> Option<u64> {
    let rest = pre.strip_prefix(channel)?;
    if rest.is_empty() {
        return Some(0);
    }
    rest.strip_prefix('.')?.parse().ok()
}

/// Display-level bump label. `prerelease_channel` is the branch's channel when
/// the branch is a prerelease branch; it decides whether `current` was already
/// on the channel or the release switches channels (labeled Minor).
fn classify_bump(current: &Version, next: &Version, prerelease_channel: Option<&str>) -> BumpLevel {
    if !next.pre.is_empty() {
        let channel_switched = prerelease_channel.is_some_and(|ch| {
            !current.pre.is_empty() && !prerelease_matches_channel(current.pre.as_str(), ch)
        });
        if next.major > current.major
            || (current.pre.is_empty() && next.minor > current.minor)
            || channel_switched
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
    fn test_classify_bump_prerelease_labels() {
        let classify = |cur: &str, next: &str, ch: Option<&str>| {
            classify_bump(
                &Version::parse(cur).unwrap(),
                &Version::parse(next).unwrap(),
                ch,
            )
        };

        // On-channel increment is a patch, including dotted channel names.
        assert_eq!(
            classify("2.0.0-beta.3", "2.0.0-beta.4", Some("beta")),
            BumpLevel::Patch
        );
        assert_eq!(
            classify("1.2.3-test-1.0", "1.2.3-test-1.0.1", Some("test-1.0")),
            BumpLevel::Patch
        );
        // Switching channels is labeled Minor.
        assert_eq!(
            classify("1.0.0-alpha.3", "1.0.1-beta.1", Some("beta")),
            BumpLevel::Minor
        );
        // From a stable base, the underlying stable delta decides.
        assert_eq!(
            classify("1.0.0", "1.0.1-beta.1", Some("beta")),
            BumpLevel::Patch
        );
        assert_eq!(
            classify("1.0.0", "1.1.0-beta.1", Some("beta")),
            BumpLevel::Minor
        );
    }

    #[test]
    fn test_channel_counter() {
        assert_eq!(channel_counter("beta.3", "beta"), Some(3));
        assert_eq!(channel_counter("beta", "beta"), Some(0));
        assert_eq!(channel_counter("test-1.0.2", "test-1.0"), Some(2));
        assert_eq!(channel_counter("test-1.0", "test-1.0"), Some(0));
        assert_eq!(channel_counter("beta.x", "beta"), None);
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
    fn test_stable_release_strips_build_metadata() {
        let v = Version::parse("1.2.3+ci.7").unwrap();
        let result = calculate_next_version(&v, &[make_commit("fix: x")], &stable_ctx()).unwrap();
        assert_eq!(result, Version::parse("1.2.4").unwrap());
    }

    #[test]
    fn test_prerelease_counter_overflow_starts_fresh() {
        // A hand-crafted tag with a u64::MAX counter must not wrap below the base.
        let current = Version::parse("1.0.0-beta.18446744073709551615").unwrap();
        let result =
            calculate_prerelease_version(&current, &[make_commit("fix: x")], "beta").unwrap();
        assert_eq!(result, Version::parse("1.0.1-beta.1").unwrap());
    }

    #[test]
    fn test_prerelease_dotted_channel_increments_in_place() {
        // Channel names containing dots (e.g. branch "test-1.0") must not be
        // split at the wrong dot.
        let commits = vec![make_commit("fix: something")];
        let current = Version::parse("1.2.3-test-1.0").unwrap();
        let result = calculate_prerelease_version(&current, &commits, "test-1.0").unwrap();
        assert_eq!(result, Version::parse("1.2.3-test-1.0.1").unwrap());

        let current = Version::parse("1.2.3-test-1.0.2").unwrap();
        let result = calculate_prerelease_version(&current, &commits, "test-1.0").unwrap();
        assert_eq!(result, Version::parse("1.2.3-test-1.0.3").unwrap());
    }

    #[test]
    fn test_prerelease_non_numeric_suffix_starts_fresh() {
        // A hand-made tag like 1.0.0-beta.x has no counter to increment; it
        // falls back to a stable bump with the channel's first counter.
        let current = Version::parse("1.0.0-beta.x").unwrap();
        let commits = vec![make_commit("fix: something")];
        let result = calculate_prerelease_version(&current, &commits, "beta").unwrap();
        assert_eq!(result, Version::parse("1.0.1-beta.1").unwrap());
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
        git::TagIndex::from_tag_versions(HashMap::new())
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
        assert_eq!(dep.propagated_from, Some(vec!["@test/assets".to_string()]));
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
        assert_eq!(
            c.propagated_from,
            Some(vec!["a".to_string(), "b".to_string()])
        );
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
        let tag_index = git::TagIndex::from_tag_versions(stable);

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
    fn test_propagate_collision_detected_despite_build_metadata() {
        let packages = vec![
            make_pkg("@test/core", &[]),
            make_pkg("@test/app", &["@test/core"]),
        ];
        let tag_infos = vec![make_tag_info("1.0.0"), make_tag_info("1.0.0")];
        let mut releases = vec![seed_release("@test/core")];

        // The colliding tag carries build metadata; it must still match.
        let mut stable: HashMap<String, HashSet<Version>> = HashMap::new();
        stable.insert(
            "@test/app".into(),
            HashSet::from([Version::parse("1.0.1+ci.7").unwrap()]),
        );
        let tag_index = git::TagIndex::from_tag_versions(stable);

        propagate_to_dependents(
            &mut releases,
            &packages,
            &tag_infos,
            &tag_index,
            &stable_ctx(),
        )
        .unwrap();

        assert!(releases.iter().all(|r| r.package_name != "@test/app"));
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
