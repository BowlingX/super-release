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
    /// Auto-detect the package manager for a repository.
    ///
    /// Detection order:
    /// 1. `packageManager` field in root `package.json` (corepack convention)
    /// 2. Lock file presence: `pnpm-lock.yaml` → pnpm, `yarn.lock` → yarn, `package-lock.json` → npm
    /// 3. Falls back to npm
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

    /// Verify the package manager is available on the system.
    pub fn verify(&self) -> Result<()> {
        let cmd = self.command_name();
        let output = Command::new(cmd).arg("--version").output();
        match output {
            Ok(o) if o.status.success() => Ok(()),
            _ => anyhow::bail!("{} is not available. Please install it.", cmd),
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

    /// Build the publish command for a package directory.
    pub fn publish_command(
        &self,
        pkg_dir: &Path,
        access: Option<&str>,
        registry: Option<&str>,
        tag: Option<&str>,
        provenance: bool,
        extra_args: &[String],
    ) -> Command {
        let mut cmd = match self {
            PackageManager::Npm => {
                let mut c = Command::new("npm");
                c.arg("publish");
                c
            }
            PackageManager::Yarn => {
                let mut c = Command::new("yarn");
                c.arg("npm").arg("publish");
                c
            }
            PackageManager::Pnpm => {
                let mut c = Command::new("pnpm");
                c.arg("publish");
                c
            }
        };

        cmd.current_dir(pkg_dir);

        if let Some(access) = access {
            cmd.arg("--access").arg(access);
        }

        if let Some(reg) = registry {
            cmd.arg("--registry").arg(reg);
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

/// Try to detect from the `packageManager` field in root package.json.
/// Format: `"yarn@4.0.0"`, `"pnpm@9.0.0"`, `"npm@10.0.0"`
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
        assert_eq!(PackageManager::detect(dir.path()).unwrap(), PackageManager::Npm);
    }

    #[test]
    fn test_detect_from_lockfile_yarn() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        std::fs::write(dir.path().join("yarn.lock"), "").unwrap();
        assert_eq!(PackageManager::detect(dir.path()).unwrap(), PackageManager::Yarn);
    }

    #[test]
    fn test_detect_from_lockfile_pnpm() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        std::fs::write(dir.path().join("pnpm-lock.yaml"), "").unwrap();
        assert_eq!(PackageManager::detect(dir.path()).unwrap(), PackageManager::Pnpm);
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
        assert_eq!(PackageManager::detect(dir.path()).unwrap(), PackageManager::Pnpm);
    }

    #[test]
    fn test_detect_fallback_npm() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        assert_eq!(PackageManager::detect(dir.path()).unwrap(), PackageManager::Npm);
    }

    #[test]
    fn test_display() {
        assert_eq!(PackageManager::Npm.to_string(), "npm");
        assert_eq!(PackageManager::Yarn.to_string(), "yarn");
        assert_eq!(PackageManager::Pnpm.to_string(), "pnpm");
    }
}
