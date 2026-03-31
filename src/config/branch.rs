use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::glob_match;

/// Configuration for a release branch.
///
/// Branches can be:
/// - **Stable** (default): `main`, `master` — produces normal releases
/// - **Prerelease**: `beta`, `next`, `alpha` — produces e.g. `2.0.0-beta.1`
/// - **Maintenance**: `1.x`, `2.x` — produces patch/minor releases for old majors
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum BranchConfig {
    Name(String),
    Full(BranchDef),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchDef {
    pub name: String,

    /// - `true`: use the branch name as the channel
    /// - `"beta"`: use a fixed channel name
    /// - absent/false: stable releases
    #[serde(default)]
    pub prerelease: PrereleaseSetting,

    #[serde(default)]
    pub maintenance: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PrereleaseSetting {
    #[default]
    Disabled,
    Flag(bool),
    Channel(String),
}

impl BranchConfig {
    pub fn name(&self) -> &str {
        match self {
            BranchConfig::Name(n) => n,
            BranchConfig::Full(def) => &def.name,
        }
    }

    pub fn resolve_prerelease(&self, actual_branch: &str) -> Option<String> {
        match self {
            BranchConfig::Name(_) => None,
            BranchConfig::Full(def) => match &def.prerelease {
                PrereleaseSetting::Disabled | PrereleaseSetting::Flag(false) => None,
                PrereleaseSetting::Flag(true) => Some(actual_branch.to_string()),
                PrereleaseSetting::Channel(ch) => Some(ch.clone()),
            },
        }
    }

    pub fn is_maintenance(&self) -> bool {
        match self {
            BranchConfig::Name(_) => false,
            BranchConfig::Full(def) => def.maintenance,
        }
    }
}

/// Resolved branch context for the current HEAD.
#[derive(Debug, Clone)]
pub struct BranchContext {
    pub branch_name: String,
    pub prerelease: Option<String>,
    pub maintenance: bool,
}

/// Detect the current branch and resolve it against the branch config.
/// Returns `None` if the current branch is not configured for releases.
pub fn resolve_branch_context(
    repo: &git2::Repository,
    branches: &[BranchConfig],
) -> Result<Option<BranchContext>> {
    let head = repo.head().context("Failed to get HEAD")?;
    let branch_name = head.shorthand().unwrap_or("HEAD").to_string();

    for bc in branches {
        if glob_match(bc.name(), &branch_name) {
            return Ok(Some(BranchContext {
                prerelease: bc.resolve_prerelease(&branch_name),
                branch_name: branch_name.clone(),
                maintenance: bc.is_maintenance(),
            }));
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_branch() {
        let bc = BranchConfig::Name("main".into());
        assert_eq!(bc.name(), "main");
        assert!(bc.resolve_prerelease("main").is_none());
        assert!(!bc.is_maintenance());
    }

    #[test]
    fn test_prerelease_fixed_channel() {
        let bc = BranchConfig::Full(BranchDef {
            name: "beta".into(),
            prerelease: PrereleaseSetting::Channel("beta".into()),
            maintenance: false,
        });
        assert_eq!(bc.resolve_prerelease("beta").as_deref(), Some("beta"));
    }

    #[test]
    fn test_prerelease_true_uses_branch_name() {
        let bc = BranchConfig::Full(BranchDef {
            name: "test-*".into(),
            prerelease: PrereleaseSetting::Flag(true),
            maintenance: false,
        });
        assert_eq!(
            bc.resolve_prerelease("test-hello").as_deref(),
            Some("test-hello")
        );
        assert_ne!(bc.resolve_prerelease("test-hello").as_deref(), Some("true"));
    }

    #[test]
    fn test_prerelease_false_is_stable() {
        let bc = BranchConfig::Full(BranchDef {
            name: "staging".into(),
            prerelease: PrereleaseSetting::Flag(false),
            maintenance: false,
        });
        assert!(bc.resolve_prerelease("staging").is_none());
    }

    #[test]
    fn test_maintenance() {
        let bc = BranchConfig::Full(BranchDef {
            name: "1.x".into(),
            prerelease: PrereleaseSetting::Disabled,
            maintenance: true,
        });
        assert!(bc.is_maintenance());
        assert!(bc.resolve_prerelease("1.x").is_none());
    }

    #[test]
    fn test_glob_match_patterns() {
        assert!(glob_match("main", "main"));
        assert!(!glob_match("main", "master"));
        assert!(glob_match("test-*", "test-foo"));
        assert!(!glob_match("test-*", "dev-foo"));
        assert!(glob_match("*.x", "2.x"));
        assert!(glob_match("@acme/*", "@acme/core"));
    }
}
