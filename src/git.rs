use anyhow::{Context, Result};
use git2::{DiffOptions, Repository, Sort};
use semver::Version;
use std::collections::HashMap;
use std::path::Path;

use crate::commit::{parse_conventional_commit, ConventionalCommit};

/// Open a git repository at the given path (searches upward).
pub fn open_repo(path: &Path) -> Result<Repository> {
    Repository::discover(path).context("Failed to open git repository")
}

/// Get all tags that look like version tags for a given package.
/// For monorepo: tags like `<package-name>@<version>` or `<package-name>/v<version>`
/// For single package: tags like `v<version>`
pub fn get_version_tags(repo: &Repository, package_name: &str) -> Result<HashMap<String, Version>> {
    let mut tags = HashMap::new();
    let tag_names = repo.tag_names(None)?;

    for tag_name in tag_names.iter().flatten() {
        // Try monorepo format: @scope/pkg@1.0.0 or pkg@1.0.0
        if let Some(version_str) = tag_name.strip_prefix(&format!("{}@", package_name)) {
            if let Ok(v) = Version::parse(version_str) {
                tags.insert(tag_name.to_string(), v);
            }
        }
        // Try v-prefixed: pkg/v1.0.0
        else if let Some(version_str) = tag_name.strip_prefix(&format!("{}/v", package_name)) {
            if let Ok(v) = Version::parse(version_str) {
                tags.insert(tag_name.to_string(), v);
            }
        }
        // Try plain v-prefix for root packages: v1.0.0
        else if package_name == "root" || package_name.is_empty() {
            if let Some(version_str) = tag_name.strip_prefix('v') {
                if let Ok(v) = Version::parse(version_str) {
                    tags.insert(tag_name.to_string(), v);
                }
            }
        }
    }

    Ok(tags)
}

/// Find the latest version tag for a package and return the tag name + version.
pub fn find_latest_version(
    repo: &Repository,
    package_name: &str,
) -> Result<Option<(String, Version)>> {
    let tags = get_version_tags(repo, package_name)?;
    Ok(tags.into_iter().max_by(|a, b| a.1.cmp(&b.1)))
}

/// Get all commits since a given tag (or all commits if tag is None).
/// Returns commits with their changed files populated.
pub fn get_commits_since(
    repo: &Repository,
    since_tag: Option<&str>,
) -> Result<Vec<ConventionalCommit>> {
    let mut revwalk = repo.revwalk()?;
    revwalk.set_sorting(Sort::TOPOLOGICAL | Sort::TIME)?;
    revwalk.push_head()?;

    // If we have a tag, hide everything reachable from it
    if let Some(tag_name) = since_tag {
        let tag_ref = repo
            .resolve_reference_from_short_name(tag_name)
            .or_else(|_| repo.find_reference(&format!("refs/tags/{}", tag_name)))?;
        let target = tag_ref.peel_to_commit()?;
        revwalk.hide(target.id())?;
    }

    let mut commits = Vec::new();

    for oid in revwalk {
        let oid = oid?;
        let commit = repo.find_commit(oid)?;

        let message = commit.message().unwrap_or("").to_string();
        let hash = oid.to_string();

        if let Some(mut parsed) = parse_conventional_commit(&hash[..8], &message) {
            // Get changed files
            parsed.files_changed = get_changed_files(repo, &commit)?;
            commits.push(parsed);
        }
    }

    Ok(commits)
}

/// Get list of files changed in a commit.
fn get_changed_files(repo: &Repository, commit: &git2::Commit) -> Result<Vec<String>> {
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

/// Create a git tag.
pub fn create_tag(repo: &Repository, tag_name: &str, message: &str) -> Result<()> {
    let head = repo.head()?.peel_to_commit()?;
    let sig = repo.signature()?;
    repo.tag(
        tag_name,
        head.as_object(),
        &sig,
        message,
        false,
    )?;
    Ok(())
}

