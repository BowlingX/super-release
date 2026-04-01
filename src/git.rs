use anyhow::{Context, Result};
use git2::Repository;
use indicatif::{ProgressBar, ProgressStyle};
use semver::Version;
use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::commit::{ConventionalCommit, parse_conventional_commit};
use crate::config::{BranchContext, Config};

/// All resolved tag information, computed once and shared across packages.
pub struct TagIndex {
    /// Tags matching per package: package_name → Vec<(tag_name, version)>
    per_package: HashMap<String, Vec<(String, Version)>>,
    /// ALL stable versions per package, regardless of branch filtering.
    /// Used to detect version collisions on maintenance branches.
    all_stable_versions: HashMap<String, HashSet<Version>>,
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
            .map(|(name, is_root)| {
                (
                    name.as_str(),
                    *is_root,
                    config.tag_match_regex(name, *is_root),
                )
            })
            .collect();

        // Collect candidate tags (matching regex + branch filter) with their OIDs
        struct Candidate {
            tag_name: String,
            tag_oid: git2::Oid,
            pkg_name: String,
            version: Version,
        }
        // First pass: find tags that match any package regex + branch filter.
        // Defer OID resolution until we know the tag is relevant.
        struct PreCandidate {
            tag_name: String,
            pkg_name: String,
            version: Version,
        }
        let mut pre_candidates: Vec<PreCandidate> = Vec::new();
        let mut matched_tag_names: HashSet<String> = HashSet::new();
        let mut all_stable_versions: HashMap<String, HashSet<Version>> = HashMap::new();

        for tag_name in tag_names.iter().flatten() {
            for (pkg_name, _is_root, tag_re) in &pkg_regexes {
                let Some(v) = extract_version_from_tag(tag_name, tag_re) else {
                    continue;
                };
                // Collect ALL stable versions (unfiltered) for collision detection.
                if v.pre.is_empty() {
                    all_stable_versions
                        .entry(pkg_name.to_string())
                        .or_default()
                        .insert(v.clone());
                }
                if !version_matches_branch(&v, branch_ctx) {
                    continue;
                }
                matched_tag_names.insert(tag_name.to_string());
                pre_candidates.push(PreCandidate {
                    tag_name: tag_name.to_string(),
                    pkg_name: pkg_name.to_string(),
                    version: v,
                });
            }
        }

        // Second pass: resolve OIDs only for matched tags.
        let mut oid_cache: HashMap<String, Option<git2::Oid>> = HashMap::new();
        for name in &matched_tag_names {
            oid_cache.insert(name.clone(), tag_to_oid(repo, name)?);
        }

        let mut candidates: Vec<Candidate> = Vec::new();
        let mut pending_oids: HashSet<git2::Oid> = HashSet::new();

        for pc in pre_candidates {
            if let Some(&Some(tag_oid)) = oid_cache.get(&pc.tag_name) {
                pending_oids.insert(tag_oid);
                candidates.push(Candidate {
                    tag_name: pc.tag_name,
                    tag_oid,
                    pkg_name: pc.pkg_name,
                    version: pc.version,
                });
            }
        }

        // Single revwalk from HEAD — stops as soon as all candidate OIDs are found.
        let mut reachable: HashSet<git2::Oid> = HashSet::new();
        if !pending_oids.is_empty() {
            let mut remaining = pending_oids;
            let mut revwalk = repo.revwalk()?;
            revwalk.push_head()?;

            for oid in revwalk {
                let oid = oid?;
                if remaining.remove(&oid) {
                    reachable.insert(oid);
                    if remaining.is_empty() {
                        break;
                    }
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

        Ok(TagIndex {
            per_package,
            all_stable_versions,
        })
    }

    /// Find the latest version tag for a package (highest semver).
    pub fn latest_version(&self, package_name: &str) -> Option<(String, Version)> {
        self.per_package
            .get(package_name)?
            .iter()
            .max_by(|a, b| a.1.cmp(&b.1))
            .cloned()
    }

    /// Find the latest prerelease tag for a specific channel.
    /// Returns `None` if there are no prerelease tags for the channel.
    pub fn latest_channel_version(
        &self,
        package_name: &str,
        channel: &str,
    ) -> Option<(String, Version)> {
        self.per_package
            .get(package_name)?
            .iter()
            .filter(|(_, v)| crate::version::prerelease_matches_channel(v.pre.as_str(), channel))
            .max_by(|a, b| a.1.cmp(&b.1))
            .cloned()
    }

    /// Check if a specific version already exists as a stable tag for a package,
    /// regardless of which branch it was released on.
    pub fn version_exists(&self, package_name: &str, version: &Version) -> bool {
        self.all_stable_versions
            .get(package_name)
            .is_some_and(|versions| versions.contains(version))
    }
}

fn extract_version_from_tag(tag_name: &str, tag_re: &Option<regex::Regex>) -> Option<Version> {
    let re = tag_re.as_ref()?;
    let caps = re.captures(tag_name)?;
    Version::parse(&caps["version"]).ok()
}

fn version_matches_branch(v: &Version, branch_ctx: &BranchContext) -> bool {
    match &branch_ctx.prerelease {
        None => v.pre.is_empty(),
        Some(channel) => {
            v.pre.is_empty() || crate::version::prerelease_matches_channel(v.pre.as_str(), channel)
        }
    }
}

/// Get all commits from HEAD, optionally stopping at a tag boundary.
/// Uses `git log --name-only` for fast file-change detection (leverages git's pack caching).
pub fn get_commits_since(
    _repo: &Repository,
    repo_path: &Path,
    since_tag: Option<&str>,
) -> Result<Vec<ConventionalCommit>> {
    use std::io::BufReader;
    use std::process::{Command, Stdio};

    // Format: each commit is separated by \x1e (record separator).
    // Within each commit: HASH\tFULL_MESSAGE\x1e\nFILE1\nFILE2\n...
    // The --name-only output follows after the format string.
    let mut cmd = Command::new("git");
    cmd.args([
        "log",
        "--format=%x1e%H%x09%B",
        "--name-only",
        "--topo-order",
    ])
    .current_dir(repo_path)
    .stdout(Stdio::piped())
    .stderr(Stdio::null());

    if let Some(tag) = since_tag {
        cmd.arg(format!("{}..HEAD", tag));
    }

    let mut child = cmd.spawn().context("Failed to run git log")?;

    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::default_spinner()
            .template("  {spinner:.cyan} Reading commits...")
            .unwrap(),
    );
    spinner.enable_steady_tick(std::time::Duration::from_millis(80));

    // Read all output at once, then split by record separator
    let mut raw_output = String::new();
    if let Some(stdout) = child.stdout.take() {
        use std::io::Read;
        let mut reader = BufReader::new(stdout);
        reader.read_to_string(&mut raw_output)?;
    }
    child.wait()?;

    let mut commits = Vec::new();

    for record in raw_output.split('\x1e') {
        let record = record.trim();
        if record.is_empty() {
            continue;
        }

        // Split into lines: first line is "HASH\tMESSAGE", then blank line, then file names
        let mut lines = record.lines();
        let Some(header) = lines.next() else {
            continue;
        };
        let Some((hash, first_msg_line)) = header.split_once('\t') else {
            continue;
        };

        // Collect message lines (until we hit a blank line followed by file names)
        let mut message_lines = vec![first_msg_line.to_string()];
        let mut files = Vec::new();
        let mut past_message = false;

        for line in lines {
            if past_message {
                if !line.is_empty() {
                    files.push(line.to_string());
                }
            } else if line.is_empty() && message_lines.last().map(|l| l.is_empty()).unwrap_or(false)
            {
                // Double blank line or blank after message = start of file list
                past_message = true;
            } else {
                message_lines.push(line.to_string());
            }
        }

        // Trim trailing empty lines from message
        while message_lines.last().map(|l| l.is_empty()).unwrap_or(false) {
            message_lines.pop();
        }

        let full_message = message_lines.join("\n");
        let hash8 = &hash[..8.min(hash.len())];

        if let Some(mut parsed) = parse_conventional_commit(hash8, &full_message) {
            parsed.oid = git2::Oid::from_str(hash).ok();
            parsed.files_changed = files;
            commits.push(parsed);
            spinner.tick();
        }
    }

    spinner.finish_and_clear();
    Ok(commits)
}

/// Fetch the remote tracking branch and check if local is behind.
/// Returns an error if the remote has commits not present locally.
pub fn check_branch_up_to_date(
    repo_root: &Path,
    repo: &Repository,
    branch_name: &str,
) -> Result<()> {
    let local_ref = match repo.find_branch(branch_name, git2::BranchType::Local) {
        Ok(b) => b,
        Err(_) => return Ok(()),
    };

    let upstream = match local_ref.upstream() {
        Ok(u) => u,
        Err(_) => return Ok(()),
    };

    // Get the remote name (e.g. "origin") from the upstream ref "refs/remotes/origin/main"
    let upstream_name = upstream.get().name().unwrap_or("");
    let remote_name = upstream_name
        .strip_prefix("refs/remotes/")
        .and_then(|s| s.split('/').next())
        .unwrap_or("origin");

    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::default_spinner()
            .template("  {spinner:.cyan} Fetching {msg}...")
            .unwrap(),
    );
    spinner.set_message(format!("{}/{}", remote_name, branch_name));
    spinner.enable_steady_tick(std::time::Duration::from_millis(80));

    let status = std::process::Command::new("git")
        .args(["fetch", remote_name, branch_name, "--quiet"])
        .current_dir(repo_root)
        .status();

    spinner.finish_and_clear();

    if let Ok(s) = status
        && !s.success()
    {
        return Ok(());
    }

    // Re-read upstream after fetch
    let local_ref = repo.find_branch(branch_name, git2::BranchType::Local)?;
    let upstream = match local_ref.upstream() {
        Ok(u) => u,
        Err(_) => return Ok(()),
    };

    let local_oid = local_ref.get().peel_to_commit()?.id();
    let remote_oid = upstream.get().peel_to_commit()?.id();

    if local_oid == remote_oid {
        return Ok(());
    }

    // Check if remote is ahead of local
    let (_, behind) = repo.graph_ahead_behind(local_oid, remote_oid)?;
    if behind > 0 {
        anyhow::bail!(
            "Local branch '{}' is {} commit(s) behind its remote. \
             Pull the latest changes before releasing.",
            branch_name,
            behind
        );
    }

    Ok(())
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
