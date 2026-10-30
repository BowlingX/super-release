use anyhow::{Context, Result};
use serde::Deserialize;
use std::fmt;
use std::path::Path;
use std::process::Command;

/// Supported package managers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageManager {
    Npm,
    Yarn,
    Pnpm,
}

/// Whether a package goes live right away, or waits in the registry's stage queue until a maintainer approves it
/// with 2FA (npm staged publishing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishMode {
    Publish,
    Stage,
}

impl PublishMode {
    pub fn verb(self) -> &'static str {
        match self {
            PublishMode::Publish => "publish",
            PublishMode::Stage => "stage",
        }
    }

    pub fn capitalized(self) -> &'static str {
        match self {
            PublishMode::Publish => "Publish",
            PublishMode::Stage => "Stage",
        }
    }

    pub fn gerund(self) -> &'static str {
        match self {
            PublishMode::Publish => "Publishing",
            PublishMode::Stage => "Staging",
        }
    }
}

impl fmt::Display for PackageManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PackageManager::Npm => write!(f, "npm"),
            PackageManager::Yarn => write!(f, "yarn"),
            PackageManager::Pnpm => write!(f, "pnpm"),
        }
    }
}

impl PackageManager {
    /// Auto-detect the package manager, preferring the `packageManager` field over lock files.
    pub fn detect(repo_root: &Path) -> Result<Self> {
        if let Some(pm) = detect_from_package_json(repo_root)? {
            return Ok(pm);
        }

        if repo_root.join("pnpm-lock.yaml").exists() {
            return Ok(PackageManager::Pnpm);
        }
        if repo_root.join("yarn.lock").exists() {
            return Ok(PackageManager::Yarn);
        }

        Ok(PackageManager::Npm)
    }

    /// Verify the package manager is available on the system, and recent enough to stage packages for `mode`
    /// [`PublishMode::Stage`]. An unparseable version only warns, so unusual builds are not blocked; the stage
    /// command itself fails if it is unsupported.
    pub fn verify(&self, mode: PublishMode) -> Result<()> {
        let cmd = self.command_name();
        let output = match Command::new(cmd).arg("--version").output() {
            Ok(o) if o.status.success() => o,
            _ => anyhow::bail!("{} is not available. Please install it.", cmd),
        };
        if mode == PublishMode::Publish {
            return Ok(());
        }

        let min = self.min_staging_version();
        match parse_version(&String::from_utf8_lossy(&output.stdout)) {
            Ok(v) if v < min => anyhow::bail!(
                "Staged publishing requires {} >= {} (found {}). Please upgrade it.",
                self,
                min,
                v
            ),
            Ok(_) => Ok(()),
            Err(e) => {
                eprintln!(
                    "  [{}] warning: could not determine the {} version ({:#}); staged publishing requires {} >= {}",
                    self, self, e, self, min
                );
                Ok(())
            }
        }
    }

    /// The oldest release that can stage packages: `npm stage publish` (npm 11.15.0),
    /// `pnpm stage publish` (pnpm 11.3.0), `yarn npm publish --staged` (yarn 4.16.0).
    pub fn min_staging_version(&self) -> semver::Version {
        match self {
            PackageManager::Npm => semver::Version::new(11, 15, 0),
            PackageManager::Pnpm => semver::Version::new(11, 3, 0),
            PackageManager::Yarn => semver::Version::new(4, 16, 0),
        }
    }

    /// The command a maintainer runs to approve (publish) a staged package. Approval needs 2FA.
    pub fn stage_approve_command(&self, stage_id: &str) -> String {
        format!("{} approve {}", self.stage_cli(), stage_id)
    }

    /// The command that lists a package's staged versions with their stage ids.
    pub fn stage_list_command(&self, package: &str) -> String {
        match self {
            PackageManager::Yarn => format!("{} list", self.stage_cli()),
            _ => format!("{} list {}", self.stage_cli(), package),
        }
    }

    /// The command group managing staged packages.
    fn stage_cli(&self) -> &'static str {
        match self {
            PackageManager::Npm => "npm stage",
            PackageManager::Pnpm => "pnpm stage",
            PackageManager::Yarn => "yarn npm stage",
        }
    }

    /// The base command name.
    pub fn command_name(&self) -> &str {
        match self {
            PackageManager::Npm => "npm",
            PackageManager::Yarn => "yarn",
            PackageManager::Pnpm => "pnpm",
        }
    }

    /// Build the publish (or, for [`PublishMode::Stage`], the stage) command for a package directory.
    pub fn publish_command(
        &self,
        pkg_dir: &Path,
        access: Option<&str>,
        tag: Option<&str>,
        provenance: bool,
        mode: PublishMode,
        extra_args: &[String],
    ) -> Command {
        let staged = mode == PublishMode::Stage;
        let mut cmd = Command::new(self.command_name());
        match self {
            PackageManager::Npm | PackageManager::Pnpm => {
                if staged {
                    cmd.arg("stage");
                }
                cmd.arg("publish");
            }
            PackageManager::Yarn => {
                cmd.arg("npm").arg("publish");
                if staged {
                    cmd.arg("--staged");
                }
            }
        }

        cmd.current_dir(pkg_dir);

        if let Some(access) = access {
            cmd.arg("--access").arg(access);
        }

        if let Some(t) = tag {
            cmd.arg("--tag").arg(t);
        }

        if provenance {
            cmd.arg("--provenance");
        }

        match self {
            PackageManager::Pnpm => {
                cmd.arg("--no-git-checks");
            }
            PackageManager::Npm => {}
            PackageManager::Yarn => {}
        }

        for arg in extra_args {
            cmd.arg(arg);
        }

        cmd
    }
}

/// Parse `--version` output such as `11.15.0` or `v4.16.0`, ignoring any notice lines before it.
fn parse_version(output: &str) -> Result<semver::Version> {
    let line = output
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .unwrap_or_default();
    semver::Version::parse(line.trim_start_matches('v'))
        .with_context(|| format!("unexpected version output {:?}", line))
}

/// Try to detect from the `packageManager` field in root package.json.
fn detect_from_package_json(repo_root: &Path) -> Result<Option<PackageManager>> {
    let manifest = repo_root.join("package.json");
    let content = match std::fs::read_to_string(&manifest) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("reading {}", manifest.display())),
    };

    #[derive(Deserialize)]
    struct Root {
        #[serde(rename = "packageManager")]
        package_manager: Option<String>,
    }

    let root: Root = serde_json::from_str(&content)
        .with_context(|| format!("parsing {}", manifest.display()))?;

    let Some(pm_str) = root.package_manager else {
        return Ok(None);
    };

    // Format: "yarn@4.0.0" or just "yarn"
    let name = pm_str.split('@').next().unwrap_or(&pm_str);
    match name {
        "yarn" => Ok(Some(PackageManager::Yarn)),
        "pnpm" => Ok(Some(PackageManager::Pnpm)),
        "npm" => Ok(Some(PackageManager::Npm)),
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_detect_from_lockfile_npm() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        std::fs::write(dir.path().join("package-lock.json"), "{}").unwrap();
        assert_eq!(
            PackageManager::detect(dir.path()).unwrap(),
            PackageManager::Npm
        );
    }

    #[test]
    fn test_detect_from_lockfile_yarn() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        std::fs::write(dir.path().join("yarn.lock"), "").unwrap();
        assert_eq!(
            PackageManager::detect(dir.path()).unwrap(),
            PackageManager::Yarn
        );
    }

    #[test]
    fn test_detect_from_lockfile_pnpm() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        std::fs::write(dir.path().join("pnpm-lock.yaml"), "").unwrap();
        assert_eq!(
            PackageManager::detect(dir.path()).unwrap(),
            PackageManager::Pnpm
        );
    }

    #[test]
    fn test_detect_from_package_manager_field() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"packageManager": "pnpm@9.0.0"}"#,
        )
        .unwrap();
        // packageManager field takes priority over lock files
        std::fs::write(dir.path().join("yarn.lock"), "").unwrap();
        assert_eq!(
            PackageManager::detect(dir.path()).unwrap(),
            PackageManager::Pnpm
        );
    }

    #[test]
    fn test_detect_fallback_npm() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        assert_eq!(
            PackageManager::detect(dir.path()).unwrap(),
            PackageManager::Npm
        );
    }

    #[test]
    fn test_parse_version() {
        assert_eq!(
            parse_version("11.15.0\n").unwrap(),
            semver::Version::new(11, 15, 0)
        );
        assert_eq!(
            parse_version("v4.16.0").unwrap(),
            semver::Version::new(4, 16, 0)
        );
        assert_eq!(
            parse_version("! Corepack is about to download pnpm\n11.3.1\n\n").unwrap(),
            semver::Version::new(11, 3, 1)
        );
        assert!(parse_version("").is_err());
        assert!(parse_version("not a version").is_err());
    }

    #[test]
    fn test_min_staging_version() {
        let below = |pm: PackageManager, v: &str| {
            semver::Version::parse(v).unwrap() < pm.min_staging_version()
        };
        assert!(below(PackageManager::Npm, "11.14.9"));
        assert!(!below(PackageManager::Npm, "11.15.0"));
        assert!(below(PackageManager::Pnpm, "11.2.0"));
        assert!(!below(PackageManager::Pnpm, "11.3.0"));
        assert!(below(PackageManager::Yarn, "1.22.22"));
        assert!(!below(PackageManager::Yarn, "4.16.0"));
    }

    #[test]
    fn test_publish_command_staged() {
        use crate::step::subprocess::format_command;
        let dir = Path::new(".");
        let stage = |pm: PackageManager| {
            format_command(&pm.publish_command(
                dir,
                Some("public"),
                Some("next"),
                true,
                PublishMode::Stage,
                &[],
            ))
        };
        assert_eq!(
            stage(PackageManager::Npm),
            "npm stage publish --access public --tag next --provenance"
        );
        assert_eq!(
            stage(PackageManager::Pnpm),
            "pnpm stage publish --access public --tag next --provenance --no-git-checks"
        );
        assert_eq!(
            stage(PackageManager::Yarn),
            "yarn npm publish --staged --access public --tag next --provenance"
        );
        assert_eq!(
            format_command(&PackageManager::Npm.publish_command(
                dir,
                None,
                None,
                false,
                PublishMode::Publish,
                &[]
            )),
            "npm publish"
        );
    }

    #[test]
    fn test_display() {
        assert_eq!(PackageManager::Npm.to_string(), "npm");
        assert_eq!(PackageManager::Yarn.to_string(), "yarn");
        assert_eq!(PackageManager::Pnpm.to_string(), "pnpm");
    }
}
