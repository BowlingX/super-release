use anyhow::Result;
use git2::{DiffOptions, Repository, Sort};
use indicatif::{ProgressBar, ProgressStyle};
use rayon::prelude::*;
use semver::Version;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::commit::{parse_conventional_commit, ConventionalCommit};
use crate::config::{BranchContext, Config};

/// Get all tags that look like version tags for a given package,
/// filtered by the current branch context:
/// - Stable branches only see stable (non-prerelease) tags.
/// - Prerelease branches see their own channel's tags AND stable tags.
/// - Maintenance branches see only their major-version range.
pub fn get_version_tags(
    repo: &Repository,
    package_name: &str,
    is_root: bool,
    config: &Config,
    branch_ctx: &BranchContext,
) -> Result<HashMap<String, Version>> {
    let mut tags = HashMap::new();
    let tag_names = repo.tag_names(None)?;

    let tag_re = config.tag_match_regex(package_name, is_root);

    for tag_name in tag_names.iter().flatten() {
        let version = extract_version_from_tag(tag_name, &tag_re);
        let Some(v) = version else { continue };

        if version_matches_branch(&v, branch_ctx) {
            tags.insert(tag_name.to_string(), v);
        }
    }

    Ok(tags)
}

/// Extract a semver Version from a tag name using the configured regex.
fn extract_version_from_tag(
    tag_name: &str,
    tag_re: &Option<regex::Regex>,
) -> Option<Version> {
    let re = tag_re.as_ref()?;
    let caps = re.captures(tag_name)?;
    Version::parse(&caps["version"]).ok()
}

/// Check if a version is relevant for the current branch context.
fn version_matches_branch(v: &Version, branch_ctx: &BranchContext) -> bool {
    match &branch_ctx.prerelease {
        None => {
            // Stable branch: only stable (non-prerelease) versions
            v.pre.is_empty()
        }
        Some(channel) => {
            // Prerelease branch: accept stable versions OR this channel's prereleases
            if v.pre.is_empty() {
                return true;
            }
            // Match if the prerelease starts with our channel
            // e.g. channel "test-foo" matches "test-foo.1", "test-foo.2"
            let pre = v.pre.as_str();
            pre == channel
                || pre.starts_with(&format!("{}.", channel))
        }
    }
}

/// Find the latest version tag for a package and return the tag name + version.
pub fn find_latest_version(
    repo: &Repository,
    package_name: &str,
    is_root: bool,
    config: &Config,
    branch_ctx: &BranchContext,
) -> Result<Option<(String, Version)>> {
    let tags = get_version_tags(repo, package_name, is_root, config, branch_ctx)?;
    Ok(tags.into_iter().max_by(|a, b| a.1.cmp(&b.1)))
}

struct RawCommit {
    oid: git2::Oid,
    parsed: ConventionalCommit,
}

/// Get all commits from HEAD, optionally stopping at a tag boundary.
/// Shows a progress bar and parallelizes diff computation with thread-local repos.
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

/// Build commits in parallel using thread-local Repository handles.
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
