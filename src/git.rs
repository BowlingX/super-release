use anyhow::Result;
use git2::{DiffOptions, Repository, Sort};
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use semver::Version;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::commit::{parse_conventional_commit, ConventionalCommit};
use crate::config::{BranchContext, Config};

/// All resolved tag information, computed once and shared across packages.
pub struct TagIndex {
    /// Tags matching per package: package_name → Vec<(tag_name, version)>
    per_package: HashMap<String, Vec<(String, Version)>>,
}

impl TagIndex {
    /// Build a tag index for all packages. Enumerates tags once, resolves
    /// reachability once, and groups by package.
    pub fn build(
        repo: &Repository,
        packages: &[(String, bool)], // (name, is_root) pairs
        config: &Config,
        branch_ctx: &BranchContext,
    ) -> Result<Self> {
        let tag_names = repo.tag_names(None)?;
        let pkg_regexes: Vec<(&str, bool, Option<regex::Regex>)> = packages
            .iter()
            .map(|(name, is_root)| (name.as_str(), *is_root, config.tag_match_regex(name, *is_root)))
            .collect();

        // Collect candidate tags (matching regex + branch filter) with their OIDs
        struct Candidate {
            tag_name: String,
            tag_oid: git2::Oid,
            pkg_name: String,
            version: Version,
        }
        let mut candidates: Vec<Candidate> = Vec::new();
        let mut pending_oids: HashSet<git2::Oid> = HashSet::new();
        let mut oid_cache: HashMap<String, Option<git2::Oid>> = HashMap::new();

        for tag_name in tag_names.iter().flatten() {
            // Resolve OID once per tag name, not per package
            let tag_oid = match oid_cache.get(tag_name) {
                Some(cached) => *cached,
                None => {
                    let oid = tag_to_oid(repo, tag_name)?;
                    oid_cache.insert(tag_name.to_string(), oid);
                    oid
                }
            };
            let Some(tag_oid) = tag_oid else { continue };

            for (pkg_name, _is_root, tag_re) in &pkg_regexes {
                let Some(v) = extract_version_from_tag(tag_name, tag_re) else {
                    continue;
                };
                if !version_matches_branch(&v, branch_ctx) {
                    continue;
                }
                pending_oids.insert(tag_oid);
                candidates.push(Candidate {
                    tag_name: tag_name.to_string(),
                    tag_oid,
                    pkg_name: pkg_name.to_string(),
                    version: v,
                });
            }
        }

        // Single revwalk from HEAD to check which candidate tag OIDs are reachable.
        // Stops as soon as all candidates are resolved OR after MAX_WALK commits
        // (tags beyond that depth are almost certainly reachable if they exist).
        const MAX_WALK: usize = 10_000;
        let mut reachable: HashSet<git2::Oid> = HashSet::new();
        if !pending_oids.is_empty() {
            let mut remaining = pending_oids.clone();
            let mut revwalk = repo.revwalk()?;
            revwalk.push_head()?;

            for (i, oid) in revwalk.enumerate() {
                let oid = oid?;
                if remaining.remove(&oid) {
                    reachable.insert(oid);
                    if remaining.is_empty() {
                        break;
                    }
                }
                if i >= MAX_WALK {
                    // Assume remaining tags are reachable (deep history)
                    reachable.extend(remaining.drain());
                    break;
                }
            }
        }

        let mut per_package: HashMap<String, Vec<(String, Version)>> = HashMap::new();
        for c in candidates {
            if reachable.contains(&c.tag_oid) {
                per_package
                    .entry(c.pkg_name)
                    .or_default()
                    .push((c.tag_name, c.version));
            }
        }

        Ok(TagIndex { per_package })
    }

    /// Find the latest version tag for a package.
    pub fn latest_version(&self, package_name: &str) -> Option<(String, Version)> {
        self.per_package
            .get(package_name)?
            .iter()
            .max_by(|a, b| a.1.cmp(&b.1))
            .cloned()
    }
}

fn extract_version_from_tag(
    tag_name: &str,
    tag_re: &Option<regex::Regex>,
) -> Option<Version> {
    let re = tag_re.as_ref()?;
    let caps = re.captures(tag_name)?;
    Version::parse(&caps["version"]).ok()
}

fn version_matches_branch(v: &Version, branch_ctx: &BranchContext) -> bool {
    match &branch_ctx.prerelease {
        None => v.pre.is_empty(),
        Some(channel) => {
            if v.pre.is_empty() {
                return true;
            }
            let pre = v.pre.as_str();
            pre == channel || pre.starts_with(&format!("{}.", channel))
        }
    }
}

struct RawCommit {
    oid: git2::Oid,
    parsed: ConventionalCommit,
}

/// Get all commits from HEAD, optionally stopping at a tag boundary.
pub fn get_commits_since(
    repo: &Repository,
    repo_path: &Path,
    since_tag: Option<&str>,
) -> Result<Vec<ConventionalCommit>> {
    let mut revwalk = repo.revwalk()?;
    revwalk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME)?;
    revwalk.push_head()?;

    if let Some(tag_name) = since_tag {
        let tag_ref = repo
            .resolve_reference_from_short_name(tag_name)
            .or_else(|_| repo.find_reference(&format!("refs/tags/{}", tag_name)))?;
        let target = tag_ref.peel_to_commit()?;
        revwalk.hide(target.id())?;
    }

    let mut raw_commits = Vec::new();
    for oid in revwalk {
        let oid = oid?;
        let commit = repo.find_commit(oid)?;
        let message = commit.message().unwrap_or("").to_string();
        let hash8 = oid.to_string()[..8].to_string();

        if let Some(parsed) = parse_conventional_commit(&hash8, &message) {
            raw_commits.push(RawCommit { oid, parsed });
        }
    }

    if raw_commits.is_empty() {
        return Ok(Vec::new());
    }

    let total = raw_commits.len();
    let pb = ProgressBar::new(total as u64);
    pb.set_style(
        ProgressStyle::default_bar()
            .template("  {spinner:.cyan} Analyzing {pos}/{len} commits [{bar:30.cyan/dim}] {eta}")
            .unwrap()
            .progress_chars("━╸─"),
    );

    let results = if total < 50 {
        raw_commits
            .into_iter()
            .map(|rc| {
                let r = build_commit(repo, rc);
                pb.inc(1);
                r
            })
            .collect::<Result<Vec<_>>>()?
    } else {
        parallel_build_commits(raw_commits, repo_path, &pb)?
    };

    pb.finish_and_clear();
    Ok(results)
}

fn parallel_build_commits(
    raw_commits: Vec<RawCommit>,
    repo_path: &Path,
    pb: &ProgressBar,
) -> Result<Vec<ConventionalCommit>> {
    let repo_path = repo_path.to_path_buf();

    thread_local! {
        static THREAD_REPO: RefCell<Option<(PathBuf, Repository)>> = const { RefCell::new(None) };
    }

    raw_commits
        .into_par_iter()
        .map(|rc| {
            let result = THREAD_REPO.with(|cell| {
                let mut slot = cell.borrow_mut();
                let repo = match slot.as_ref() {
                    Some((path, repo)) if path == &repo_path => repo,
                    _ => {
                        let new_repo = Repository::open(&repo_path)
                            .map_err(|e| anyhow::anyhow!("Failed to open repo in thread: {}", e))?;
                        *slot = Some((repo_path.clone(), new_repo));
                        &slot.as_ref().unwrap().1
                    }
                };
                build_commit(repo, rc)
            });
            pb.inc(1);
            result
        })
        .collect()
}

fn build_commit(repo: &Repository, rc: RawCommit) -> Result<ConventionalCommit> {
    let mut parsed = rc.parsed;
    parsed.files_changed = get_changed_files(repo, rc.oid)?;
    Ok(parsed)
}

fn get_changed_files(repo: &Repository, oid: git2::Oid) -> Result<Vec<String>> {
    let commit = repo.find_commit(oid)?;
    let tree = commit.tree()?;
    let parent_tree = if commit.parent_count() > 0 {
        Some(commit.parent(0)?.tree()?)
    } else {
        None
    };

    let mut opts = DiffOptions::new();
    let diff = repo.diff_tree_to_tree(parent_tree.as_ref(), Some(&tree), Some(&mut opts))?;

    let mut files = Vec::new();
    diff.foreach(
        &mut |delta, _| {
            if let Some(path) = delta.new_file().path() {
                files.push(path.to_string_lossy().to_string());
            }
            true
        },
        None,
        None,
        None,
    )?;

    Ok(files)
}

pub fn create_tag(repo: &Repository, tag_name: &str, message: &str) -> Result<()> {
    let head = repo.head()?.peel_to_commit()?;
    let sig = repo.signature()?;
    repo.tag(tag_name, head.as_object(), &sig, message, false)?;
    Ok(())
}

pub fn tag_to_oid(repo: &Repository, tag_name: &str) -> Result<Option<git2::Oid>> {
    match repo.resolve_reference_from_short_name(tag_name) {
        Ok(reference) => {
            let commit = reference.peel_to_commit()?;
            Ok(Some(commit.id()))
        }
        Err(_) => Ok(None),
    }
}
