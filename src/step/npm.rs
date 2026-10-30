use anyhow::Result;
use rayon::prelude::*;
use regex::Regex;
use serde::Deserialize;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::LazyLock;

use super::{Step, StepConfig, StepContext, parse_options, subprocess};
use crate::package::{Package, topological_sort};
use crate::pm::{PackageManager, PublishMode};
use crate::version::PackageRelease;

/// Options for the npm/publish step.
#[derive(Debug, Clone, Deserialize)]
pub struct NpmOptions {
    /// Access level for publish ("public" or "restricted"). If unset, npm's default applies.
    #[serde(default)]
    pub access: Option<String>,

    #[serde(default)]
    pub registry: Option<String>,

    #[serde(default)]
    pub publish_args: Vec<String>,

    /// Dist-tag override. By default derived from the prerelease channel.
    #[serde(default)]
    pub tag: Option<String>,

    /// Enable npm provenance (--provenance). Requires npm 9.5+ and a supported CI.
    #[serde(default)]
    pub provenance: bool,

    /// Force a specific package manager (overrides auto-detection).
    #[serde(default)]
    pub package_manager: Option<PackageManager>,

    /// Check the registry before publishing to skip already-published versions (default: true).
    #[serde(default = "default_check_registry")]
    pub check_registry: bool,

    /// Stage packages for maintainer approval (npm staged publishing) instead of publishing them directly.
    #[serde(default)]
    pub staged: bool,
}

fn default_check_registry() -> bool {
    true
}

impl Default for NpmOptions {
    fn default() -> Self {
        Self {
            access: None,
            registry: None,
            publish_args: Vec::new(),
            tag: None,
            provenance: false,
            package_manager: None,
            check_registry: true,
            staged: false,
        }
    }
}

impl NpmOptions {
    fn mode(&self) -> PublishMode {
        if self.staged {
            PublishMode::Stage
        } else {
            PublishMode::Publish
        }
    }
}

pub struct NpmStep;

impl Step for NpmStep {
    fn name(&self) -> &str {
        "npm"
    }

    fn verify(&self, ctx: &StepContext, config: &StepConfig) -> Result<()> {
        if ctx.dry_run {
            return Ok(());
        }
        let opts: NpmOptions = parse_options(config)?;
        resolve_pm(ctx, &opts)?.verify(opts.mode())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "predates the limit; split when next touched"
    )]
    fn publish(
        &self,
        ctx: &StepContext,
        config: &StepConfig,
        packages: &[Package],
        releases: &[PackageRelease],
    ) -> Result<Vec<std::path::PathBuf>> {
        let opts: NpmOptions = parse_options(config)?;
        let pm = resolve_pm(ctx, &opts)?;
        let dist_tag: Option<&str> = opts.tag.as_deref().or(ctx.branch.channel.as_deref());
        let mode = opts.mode();

        let order = topological_sort(packages)?;
        let release_set: HashMap<&str, &PackageRelease> = releases
            .iter()
            .map(|r| (r.package_name.as_str(), r))
            .collect();

        let repo_root = ctx.repo_root;
        let dry_run = ctx.dry_run;
        let mut errors: Vec<String> = Vec::new();

        let to_publish: HashMap<&str, &PackageRelease> = if opts.check_registry {
            println!("  [{}] Checking registry for published versions...", pm);

            let check_results: Vec<(&str, VersionCheckResult)> = releases
                .par_iter()
                .map(|r| {
                    let version_str = r.next_version.to_string();
                    let view_cmd_str = format_view_command(
                        &r.package_name,
                        &version_str,
                        opts.registry.as_deref(),
                    );
                    println!(
                        "    {} {}",
                        console::style(&r.package_name).bold(),
                        console::style(&view_cmd_str).dim()
                    );
                    let result =
                        run_version_check(&r.package_name, &version_str, opts.registry.as_deref());
                    (r.package_name.as_str(), result)
                })
                .collect();

            let mut to_publish = HashMap::new();
            for (name, result) in &check_results {
                let display = match result {
                    VersionCheckResult::Published(v) => format!("→ {} (published)", v),
                    VersionCheckResult::NotFound(v) => format!("→ {} (not found)", v),
                    VersionCheckResult::Error(e) => format!("→ error: {}", e),
                };
                println!(
                    "    {} {}",
                    console::style(name).bold(),
                    console::style(&display).dim()
                );

                match result {
                    VersionCheckResult::Published(_) => {
                        let ver = release_set
                            .get(name)
                            .map(|r| r.next_version.to_string())
                            .unwrap_or_default();
                        println!("  [{}] {} v{} already published, skipping", pm, name, ver);
                    }
                    VersionCheckResult::NotFound(_) => {
                        if let Some(&r) = release_set.get(name) {
                            to_publish.insert(*name, r);
                        }
                    }
                    VersionCheckResult::Error(e) => {
                        errors.push(format!("Registry check failed for {}: {}", name, e));
                    }
                }
            }

            if !errors.is_empty() {
                anyhow::bail!("Registry check failed:\n  {}", errors.join("\n  "));
            }

            to_publish
        } else {
            release_set.clone()
        };

        if to_publish.is_empty() {
            println!("  [{}] All packages already published", pm);
            return Ok(Vec::new());
        }

        let publish_levels = dependency_levels(&order, packages, &to_publish);
        if publish_levels.len() > 1 || publish_levels.first().map(|l| l.len() > 1).unwrap_or(false)
        {
            println!("  [{}] {} order:", pm, mode.capitalized());
            for (i, level) in publish_levels.iter().enumerate() {
                if !level.is_empty() {
                    let parallel = if level.len() > 1 { " (parallel)" } else { "" };
                    println!(
                        "    {} {}{}",
                        console::style(format!("{}.", i + 1)).dim(),
                        level.join(", "),
                        console::style(parallel).dim()
                    );
                }
            }
        }

        // A dependent of a package that failed is not published either, so it never goes live pointing at a
        // dependency version that does not exist. The step still fails, so a re-run retries both.
        let mut failed: HashSet<&str> = HashSet::new();
        let mut staged: Vec<StagedPackage> = Vec::new();

        for level in &publish_levels {
            let mut runnable: Vec<(&Package, &PackageRelease)> = Vec::new();
            for name in level {
                let (Some(pkg), Some(&release)) = (
                    packages.iter().find(|p| p.name == *name),
                    to_publish.get(name.as_str()),
                ) else {
                    continue;
                };
                if let Some(dep) = failed_dependency(pkg, &failed) {
                    let message = format!(
                        "{} skipped: dependency {} failed to {}",
                        pkg.name,
                        dep,
                        mode.verb()
                    );
                    eprintln!("  [{}] {}", pm, message);
                    errors.push(message);
                    failed.insert(&pkg.name);
                    continue;
                }
                runnable.push((pkg, release));
            }

            let results: Vec<(&Package, Result<Option<StagedPackage>>)> = runnable
                .par_iter()
                .map(|&(pkg, release)| {
                    let result = publish_one(repo_root, dry_run, pkg, release, &opts, pm, dist_tag);
                    (pkg, result)
                })
                .collect();

            for (pkg, result) in results {
                match result {
                    Ok(pending) => staged.extend(pending),
                    Err(e) => {
                        eprintln!("  [{}] Error: {}", pm, e);
                        errors.push(e.to_string());
                        failed.insert(&pkg.name);
                    }
                }
            }
        }

        if !staged.is_empty() {
            print_approval_summary(pm, &staged);
        }

        if errors.is_empty() {
            Ok(Vec::new())
        } else {
            anyhow::bail!(
                "{} package(s) failed to {}:\n  {}",
                errors.len(),
                mode.verb(),
                errors.join("\n  ")
            )
        }
    }
}

/// A package waiting in the registry's stage queue for a maintainer to approve it.
struct StagedPackage<'a> {
    pkg: &'a Package,
    version: &'a semver::Version,
    /// `None` when it was staged by an earlier run or the package manager did not print the id.
    stage_id: Option<String>,
}

/// The dependencies installed along with `pkg`: `dependencies` and `optionalDependencies`, not `devDependencies`.
fn runtime_dependencies(pkg: &Package) -> impl Iterator<Item = &str> {
    pkg.dependencies
        .keys()
        .chain(pkg.optional_dependencies.keys())
        .map(String::as_str)
}

/// The first (by name) runtime dependency of `pkg` that failed or was skipped earlier in this run.
fn failed_dependency<'a>(pkg: &'a Package, failed: &HashSet<&str>) -> Option<&'a str> {
    runtime_dependencies(pkg)
        .filter(|d| failed.contains(d))
        .min()
}

/// Staged versions only go live once approved with 2FA, and the registry does not check that a dependency
/// is live before approving its dependent. Lists them in dependency order with the commands to approve them.
fn print_approval_summary(pm: PackageManager, staged: &[StagedPackage]) {
    let staged_names: HashSet<&str> = staged.iter().map(|s| s.pkg.name.as_str()).collect();

    println!(
        "  [{}] {} package(s) staged, awaiting approval (requires 2FA). Approve them in this order:",
        pm,
        staged.len()
    );
    for (i, s) in staged.iter().enumerate() {
        let action = match &s.stage_id {
            Some(id) => pm.stage_approve_command(id),
            None => format!(
                "stage id unknown, find it with `{}`",
                pm.stage_list_command(&s.pkg.name)
            ),
        };
        println!(
            "    {} {}@{}  {}",
            console::style(format!("{}.", i + 1)).dim(),
            console::style(&s.pkg.name).bold(),
            s.version,
            console::style(action).dim()
        );

        let after: BTreeSet<&str> = runtime_dependencies(s.pkg)
            .filter(|d| staged_names.contains(d))
            .collect();
        if !after.is_empty() {
            let after: Vec<&str> = after.into_iter().collect();
            println!(
                "       {}",
                console::style(format!("approve after: {}", after.join(", "))).dim()
            );
        }
    }
    println!(
        "  [{}] Or approve them on npmjs.com in each package's \"Staged Packages\" tab.",
        pm
    );
}

fn resolve_pm(ctx: &StepContext, opts: &NpmOptions) -> Result<PackageManager> {
    match opts.package_manager {
        Some(pm) => Ok(pm),
        None => PackageManager::detect(ctx.repo_root),
    }
}

/// Publishes (or stages) one package. Returns it when it awaits approval: staged now, or found already staged.
fn publish_one<'a>(
    repo_root: &std::path::Path,
    dry_run: bool,
    pkg: &'a Package,
    release: &'a PackageRelease,
    opts: &NpmOptions,
    pm: PackageManager,
    dist_tag: Option<&str>,
) -> Result<Option<StagedPackage<'a>>> {
    let mode = opts.mode();
    let pkg_dir = repo_root.join(&pkg.path);
    let version = release.next_version.to_string();
    let label = format!("{} v{}", pkg.name, version);
    let pm_name = pm.to_string();

    let cmd = pm.publish_command(
        &pkg_dir,
        opts.access.as_deref(),
        dist_tag,
        opts.provenance,
        mode,
        &opts.publish_args,
    );

    if dry_run {
        println!(
            "  [{}] Would {} {}: {}",
            pm_name,
            mode.verb(),
            label,
            subprocess::format_command(&cmd)
        );
        println!(
            "    {}",
            console::style(format!("in {}", pkg_dir.display())).dim()
        );
        return Ok(None);
    }

    println!(
        "  [{}] {} {}: {}",
        pm_name,
        mode.gerund(),
        label,
        subprocess::format_command(&cmd)
    );
    println!(
        "    {}",
        console::style(format!("in {}", pkg_dir.display())).dim()
    );
    let pending = |stage_id| StagedPackage {
        pkg,
        version: &release.next_version,
        stage_id,
    };
    match subprocess::run_command(
        cmd,
        &subprocess::RunOptions {
            label: &label,
            step_name: &pm_name,
        },
    ) {
        Ok(output) if mode == PublishMode::Stage => Ok(Some(pending(extract_stage_id(&output)))),
        Ok(_) => Ok(None),
        // Checked first: the version slot is taken by a staged version, which a direct publish hits too.
        Err(failure) if is_stage_conflict(&failure.output, &version) => {
            match mode {
                PublishMode::Stage => println!(
                    "  [{}] {} already staged (pending approval), skipping",
                    pm_name, label
                ),
                PublishMode::Publish => eprintln!(
                    "  [{}] warning: {} is already staged and awaits approval, skipping the publish",
                    pm_name, label
                ),
            }
            Ok(Some(pending(None)))
        }
        Err(failure) if is_publish_conflict(&failure.output) => {
            println!(
                "  [{}] {} already published, skipping (registry rejected the republish)",
                pm_name, label
            );
            Ok(None)
        }
        Err(failure) => Err(failure.report()),
    }
}

fn format_view_command(name: &str, version: &str, registry: Option<&str>) -> String {
    let mut cmd = std::process::Command::new("npm");
    cmd.args(["view", &format!("{}@{}", name, version), "version"]);
    if let Some(reg) = registry {
        cmd.args(["--registry", reg]);
    }
    subprocess::format_command(&cmd)
}

/// Whether a failed `publish` was rejected because the version already exists on the registry.
/// Covers npm's `EPUBLISHCONFLICT` and the 403 wording npm and pnpm relay from the registry.
fn is_publish_conflict(output: &[String]) -> bool {
    output.iter().any(|line| {
        line.contains("previously published version") || line.contains("EPUBLISHCONFLICT")
    })
}

/// Whether a failed publish or stage was rejected because this version is already staged: staged and
/// published versions share one version slot until a maintainer approves or rejects the staged one.
/// The npm registry answers `409 Conflict - Cannot publish over previously staged version "1.1.0"`,
/// Verdaccio `409 Conflict - pkg@1.1.0 is already staged`; npm and pnpm relay either.
fn is_stage_conflict(output: &[String], version: &str) -> bool {
    output.iter().any(|line| {
        let line = line.to_lowercase();
        line.contains("previously staged version") || line.contains("is already staged")
    }) && output.iter().any(|line| contains_version(line, version))
}

/// Whether `line` mentions exactly `version`, so `1.1.0` matches neither `11.1.0` nor `1.1.0-beta.1`.
fn contains_version(line: &str, version: &str) -> bool {
    line.match_indices(version).any(|(i, _)| {
        let before = line[..i].chars().next_back();
        let mut after = line[i + version.len()..].chars();
        let starts = !before.is_some_and(|c| c.is_ascii_digit() || c == '.');
        let ends = match after.next() {
            None => true,
            // A sentence-ending dot rather than another version component.
            Some('.') => !after.next().is_some_and(|c| c.is_ascii_alphanumeric()),
            Some(c) => !(c.is_ascii_alphanumeric() || c == '-' || c == '+'),
        };
        starts && ends
    })
}

/// The stage id (a hyphenated UUID) printed by a successful stage, e.g. npm's
/// `+ pkg@1.1.0 (staged with id 1de6f3db-2ed9-4d72-b3dd-8f0e2b474a2f)`.
fn extract_stage_id(output: &[String]) -> Option<String> {
    static STAGE_ID_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"\b[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}\b",
        )
        .expect("stage id regex is invalid — this is a bug")
    });
    let ids: Vec<(&String, &str)> = output
        .iter()
        .filter_map(|line| Some((line, STAGE_ID_RE.find(line)?.as_str())))
        .collect();
    // Prefer the line announcing the stage over any other id-like output (e.g. provenance logs).
    ids.iter()
        .find(|(line, _)| line.to_lowercase().contains("stage"))
        .or(ids.first())
        .map(|(_, id)| id.to_string())
}

enum VersionCheckResult {
    Published(String),
    /// 404: version not found, or a private package the caller cannot read
    /// (in which case the publish itself decides, see [`is_publish_conflict`]).
    NotFound(String),
    /// Registry error (auth, network, etc.) — should not proceed.
    Error(String),
}

fn run_version_check(name: &str, version: &str, registry: Option<&str>) -> VersionCheckResult {
    let mut cmd = std::process::Command::new("npm");
    cmd.args(["view", &format!("{}@{}", name, version), "version"]);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    if let Some(reg) = registry {
        cmd.args(["--registry", reg]);
    }

    let Ok(output) = cmd.output() else {
        return VersionCheckResult::Error("failed to run npm view".into());
    };

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    if output.status.success() {
        if stdout == version {
            return VersionCheckResult::Published(stdout);
        }
        // Version not matched — treat as not found
        return VersionCheckResult::NotFound(stdout);
    }

    if stderr.contains("E404") {
        VersionCheckResult::NotFound(stderr)
    } else {
        VersionCheckResult::Error(stderr)
    }
}

fn dependency_levels(
    topo_order: &[String],
    packages: &[Package],
    release_set: &HashMap<&str, &PackageRelease>,
) -> Vec<Vec<String>> {
    let mut levels: Vec<Vec<String>> = Vec::new();
    let mut pkg_level: HashMap<&str, usize> = HashMap::new();

    for name in topo_order {
        if !release_set.contains_key(name.as_str()) {
            continue;
        }

        let pkg = packages.iter().find(|p| p.name == *name);
        let level = pkg
            .map(|p| {
                p.local_dependencies
                    .keys()
                    .filter_map(|dep| pkg_level.get(dep.as_str()))
                    .max()
                    .map(|l| l + 1)
                    .unwrap_or(0)
            })
            .unwrap_or(0);

        pkg_level.insert(name, level);
        while levels.len() <= level {
            levels.push(Vec::new());
        }
        levels[level].push(name.clone());
    }

    levels
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_fixtures::make_pkg;

    fn lines(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn test_is_publish_conflict() {
        assert!(is_publish_conflict(&lines(&[
            "npm error code E403",
            "npm error 403 403 Forbidden - PUT https://registry.npmjs.org/my-app - You cannot publish over the previously published versions: 1.1.0.",
        ])));
        assert!(is_publish_conflict(&lines(&[
            "Error: ERR_PNPM_FAILED_TO_PUBLISH",
            "  × Failed to publish package @scope/pkg@0.2.0 (status 403 Forbidden):",
            "  │ You cannot publish over the previously published versions: 0.2.0.",
        ])));
        assert!(is_publish_conflict(&lines(&[
            "npm error code EPUBLISHCONFLICT"
        ])));
        assert!(!is_publish_conflict(&lines(&[
            "  × Failed to publish package @scope/pkg@0.2.0 (status 404 Not Found):",
            "  │ {\"error\":\"Not found\"}",
        ])));
        assert!(!is_publish_conflict(&lines(&["npm error code E401"])));
    }

    #[test]
    fn test_is_stage_conflict() {
        assert!(is_stage_conflict(
            &lines(&[
                "npm error code E409",
                "npm error 409 Conflict - PUT https://registry.npmjs.org/my-app - Cannot publish over previously staged version \"1.1.0\"",
            ]),
            "1.1.0"
        ));
        // Verdaccio's wording, as npm and pnpm print it.
        assert!(is_stage_conflict(
            &lines(&[
                "npm error code E409",
                "npm error 409 Conflict - POST http://127.0.0.1:4873/-/stage/package/@sr%2fa - @sr/a@1.0.0 is already staged",
            ]),
            "1.0.0"
        ));
        assert!(is_stage_conflict(
            &lines(&[
                "Error: ERR_PNPM_FAILED_TO_PUBLISH",
                "  × Failed to stage package @sr/a@1.2.0 (status 409 Conflict)",
                "  │ Details:",
                "  │     {",
                "  │       \"error\": \"@sr/a@1.2.0 is already staged\"",
                "  │     }",
            ]),
            "1.2.0"
        ));
        assert!(!is_stage_conflict(
            &lines(&[
                "npm error 409 Conflict - Cannot publish over previously staged version \"11.1.0\""
            ]),
            "1.1.0"
        ));
        assert!(!is_stage_conflict(
            &lines(&[
                "npm error 403 403 Forbidden - PUT https://registry.npmjs.org/my-app - You cannot publish over the previously published versions: 1.1.0.",
            ]),
            "1.1.0"
        ));
    }

    #[test]
    fn test_contains_version() {
        assert!(contains_version("version \"1.1.0\"", "1.1.0"));
        assert!(contains_version("versions: 1.1.0.", "1.1.0"));
        assert!(contains_version("my-app@1.1.0 (staged)", "1.1.0"));
        assert!(!contains_version("version \"11.1.0\"", "1.1.0"));
        assert!(!contains_version("version 1.1.0-beta.1", "1.1.0"));
        assert!(!contains_version("version 1.1.0.1", "1.1.0"));
        assert!(contains_version("version 1.1.0-beta.1", "1.1.0-beta.1"));
    }

    #[test]
    fn test_extract_stage_id() {
        assert_eq!(
            extract_stage_id(&lines(&[
                "npm notice Staging to https://registry.npmjs.org/ with tag latest and default access",
                "+ my-app@1.1.0 (staged with id 1de6f3db-2ed9-4d72-b3dd-8f0e2b474a2f)",
            ])),
            Some("1de6f3db-2ed9-4d72-b3dd-8f0e2b474a2f".to_string())
        );
        assert_eq!(
            extract_stage_id(&lines(&[
                "request id 00000000-1111-2222-3333-444444444444",
                "Staged with id 123e4567-e89b-12d3-a456-426614174000",
            ])),
            Some("123e4567-e89b-12d3-a456-426614174000".to_string())
        );
        assert_eq!(extract_stage_id(&lines(&["+ my-app@1.1.0 (staged)"])), None);
    }

    #[test]
    fn test_failed_dependency_ignores_dev_dependencies() {
        let mut app = make_pkg("app", &["core", "tooling"]);
        app.dependencies.insert("core".into(), "^1.0.0".into());
        app.dev_dependencies
            .insert("tooling".into(), "^1.0.0".into());

        assert_eq!(failed_dependency(&app, &["tooling"].into()), None);
        assert_eq!(
            failed_dependency(&app, &["core", "tooling"].into()),
            Some("core")
        );

        let mut cli = make_pkg("cli", &["cli-linux-x64"]);
        cli.optional_dependencies
            .insert("cli-linux-x64".into(), "workspace:*".into());
        assert_eq!(
            failed_dependency(&cli, &["cli-linux-x64"].into()),
            Some("cli-linux-x64")
        );
    }

    #[test]
    fn test_dependency_levels() {
        let packages = vec![
            make_pkg("a", &[]),
            make_pkg("b", &["a"]),
            make_pkg("c", &["a"]),
            make_pkg("d", &["b", "c"]),
        ];

        let releases: Vec<PackageRelease> = ["a", "b", "c", "d"]
            .iter()
            .map(|n| PackageRelease {
                package_name: n.to_string(),
                current_version: semver::Version::new(1, 0, 0),
                next_version: semver::Version::new(1, 1, 0),
                bump: crate::commit::BumpLevel::Minor,
                commits: vec![],
                is_root: false,
                propagated_from: None,
            })
            .collect();

        let release_set: HashMap<&str, &PackageRelease> = releases
            .iter()
            .map(|r| (r.package_name.as_str(), r))
            .collect();

        let order = topological_sort(&packages).unwrap();
        let levels = dependency_levels(&order, &packages, &release_set);

        assert_eq!(levels.len(), 3);
        assert_eq!(levels[0], vec!["a"]);
        assert!(levels[1].contains(&"b".to_string()));
        assert!(levels[1].contains(&"c".to_string()));
        assert_eq!(levels[2], vec!["d"]);
    }
}
