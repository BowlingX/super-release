use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Top-level configuration for super-release.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Branch configurations for release channels.
    #[serde(default = "default_branches")]
    pub branches: Vec<BranchConfig>,

    /// Tag format template for the root package (default: "v{version}").
    /// Supports `{version}` and `{name}` placeholders.
    #[serde(default = "default_tag_format")]
    pub tag_format: String,

    /// Tag format template for sub-packages (default: "{name}/v{version}").
    /// Supports `{version}` and `{name}` placeholders.
    #[serde(default = "default_tag_format_package")]
    pub tag_format_package: String,

    /// Ordered list of plugins to execute.
    #[serde(default = "default_plugins")]
    pub plugins: Vec<PluginConfig>,

    /// Packages to include (glob patterns). Default: all discovered packages.
    #[serde(default)]
    pub packages: Option<Vec<String>>,

    /// Packages to exclude (glob patterns).
    #[serde(default)]
    pub exclude: Vec<String>,
}

/// Configuration for a release branch.
///
/// Branches can be:
/// - **Stable** (default): `main`, `master` — produces normal releases
/// - **Prerelease**: `beta`, `next`, `alpha` — produces e.g. `2.0.0-beta.1`
/// - **Maintenance**: `1.x`, `2.x` — produces patch/minor releases for old majors
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum BranchConfig {
    /// Simple branch name (stable channel, no prerelease).
    Name(String),
    /// Full branch configuration with optional channel/prerelease/maintenance.
    Full(BranchDef),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BranchDef {
    /// Branch name or glob pattern (e.g. "main", "beta", "test-*", "1.x").
    pub name: String,

    /// Prerelease channel configuration.
    /// - `true`:     use the branch name as the channel (e.g. branch `test-foo` → `1.2.0-test-foo.1`)
    /// - `"beta"`:   use a fixed channel name (e.g. `1.2.0-beta.1`)
    /// - absent/false: stable releases
    #[serde(default)]
    pub prerelease: PrereleaseSetting,

    /// Whether this is a maintenance branch (e.g. "1.x").
    /// When true, the major version is capped to the number in the branch name.
    /// Breaking changes are demoted to minor bumps.
    #[serde(default)]
    pub maintenance: bool,
}

/// How to determine the prerelease channel for a branch.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PrereleaseSetting {
    /// Not a prerelease branch.
    #[default]
    Disabled,
    /// `true` means use the branch name as the prerelease channel.
    Flag(bool),
    /// A fixed channel name (e.g. "beta", "rc").
    Channel(String),
}

impl BranchConfig {
    pub fn name(&self) -> &str {
        match self {
            BranchConfig::Name(n) => n,
            BranchConfig::Full(def) => &def.name,
        }
    }

    /// Resolve the prerelease channel for a given actual branch name.
    /// Returns `None` for stable branches.
    pub fn resolve_prerelease(&self, actual_branch: &str) -> Option<String> {
        match self {
            BranchConfig::Name(_) => None,
            BranchConfig::Full(def) => match &def.prerelease {
                PrereleaseSetting::Disabled => None,
                PrereleaseSetting::Flag(false) => None,
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
    /// The current branch name.
    pub branch_name: String,
    /// Prerelease channel (e.g. "beta"), or None for stable.
    pub prerelease: Option<String>,
    /// Whether this is a maintenance branch.
    pub maintenance: bool,
}

/// Configuration for a single plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginConfig {
    /// Plugin name (e.g., "changelog", "npm", "git-tag")
    pub name: String,

    /// Plugin-specific options
    #[serde(default)]
    pub options: serde_json::Value,
}

fn default_branches() -> Vec<BranchConfig> {
    vec![
        BranchConfig::Name("main".into()),
        BranchConfig::Name("master".into()),
    ]
}

fn default_tag_format() -> String {
    "v{version}".into()
}

fn default_tag_format_package() -> String {
    "{name}/v{version}".into()
}

fn default_plugins() -> Vec<PluginConfig> {
    vec![
        PluginConfig {
            name: "changelog".into(),
            options: serde_json::Value::Null,
        },
        PluginConfig {
            name: "npm".into(),
            options: serde_json::Value::Null,
        },
        PluginConfig {
            name: "git-commit".into(),
            options: serde_json::Value::Null,
        },
        PluginConfig {
            name: "git-tag".into(),
            options: serde_json::Value::Null,
        },
    ]
}

impl Default for Config {
    fn default() -> Self {
        Config {
            branches: default_branches(),
            tag_format: default_tag_format(),
            tag_format_package: default_tag_format_package(),
            plugins: default_plugins(),
            packages: None,
            exclude: Vec::new(),
        }
    }
}

/// Load configuration from a .release.yaml file, falling back to defaults.
pub fn load_config(repo_root: &Path) -> Result<Config> {
    let candidates = [
        ".release.yaml",
        ".release.yml",
        ".super-release.yaml",
        ".super-release.yml",
    ];

    for candidate in &candidates {
        let path = repo_root.join(candidate);
        match std::fs::read_to_string(&path) {
            Ok(content) => {
                let config: Config = serde_saphyr::from_str(&content)
                    .with_context(|| format!("parsing config file: {}", path.display()))?;
                return Ok(config);
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(anyhow::anyhow!("reading {}: {}", path.display(), e)),
        }
    }

    Ok(Config::default())
}

/// Resolve the repository root from a starting path.
/// Returns both the root path and the opened Repository to avoid re-opening it.
pub fn find_repo_root(start: &Path) -> Result<(PathBuf, git2::Repository)> {
    let repo = git2::Repository::discover(start)?;
    let workdir = repo
        .workdir()
        .context("Bare repositories are not supported")?
        .to_path_buf();
    Ok((workdir, repo))
}

/// Detect the current branch and resolve it against the branch config.
/// Returns `None` if the current branch is not configured for releases.
pub fn resolve_branch_context(
    repo: &git2::Repository,
    config: &Config,
) -> Result<Option<BranchContext>> {
    let head = repo.head().context("Failed to get HEAD")?;
    let branch_name = head
        .shorthand()
        .unwrap_or("HEAD")
        .to_string();

    for bc in &config.branches {
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

/// Match a string against a glob pattern using the `glob-match` crate.
/// Supports `*`, `?`, `[...]` character classes, and `{a,b}` alternations.
///
/// Examples: `"@acme/*"` matches `"@acme/core"`, `"test-*"` matches `"test-foo"`.
pub fn glob_match(pattern: &str, value: &str) -> bool {
    glob_match::glob_match(pattern, value)
}

impl Config {
    fn tag_template(&self, is_root: bool) -> &str {
        if is_root { &self.tag_format } else { &self.tag_format_package }
    }

    pub fn format_tag(&self, package_name: &str, version: &semver::Version, is_root: bool) -> String {
        render_tag_template(self.tag_template(is_root), package_name, &version.to_string())
    }

    pub fn tag_match_regex(&self, package_name: &str, is_root: bool) -> Option<regex::Regex> {
        tag_template_to_regex(self.tag_template(is_root), package_name)
    }
}

fn render_tag_template(template: &str, name: &str, version: &str) -> String {
    template
        .replace("{name}", name)
        .replace("{version}", version)
}

/// Convert a tag template like `{name}/v{version}` into a regex that captures
/// the version: `^@acme/core/v(?P<version>.+)$`
fn tag_template_to_regex(template: &str, package_name: &str) -> Option<regex::Regex> {
    if !template.contains("{version}") {
        return None;
    }
    let escaped = regex::escape(&template.replace("{name}", package_name));
    let pattern = escaped.replace(r"\{version\}", r"(?P<version>[0-9].+)");
    regex::Regex::new(&format!("^{}$", pattern)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.branches.len(), 2);
        assert_eq!(config.branches[0].name(), "main");
        assert_eq!(config.branches[1].name(), "master");
        assert_eq!(config.tag_format, "v{version}");
        assert_eq!(config.tag_format_package, "{name}/v{version}");
        assert_eq!(config.plugins.len(), 4);
    }

    #[test]
    fn test_format_tag_root() {
        let config = Config::default();
        let v = semver::Version::new(1, 2, 3);
        assert_eq!(config.format_tag("my-app", &v, true), "v1.2.3");
    }

    #[test]
    fn test_format_tag_subpackage() {
        let config = Config::default();
        let v = semver::Version::new(1, 2, 3);
        assert_eq!(config.format_tag("@myorg/core", &v, false), "@myorg/core/v1.2.3");
    }

    #[test]
    fn test_format_tag_custom_templates() {
        let config = Config {
            tag_format: "release-{version}".into(),
            tag_format_package: "{name}@{version}".into(),
            ..Config::default()
        };
        let v = semver::Version::new(2, 0, 0);
        assert_eq!(config.format_tag("my-app", &v, true), "release-2.0.0");
        assert_eq!(config.format_tag("@acme/lib", &v, false), "@acme/lib@2.0.0");
    }

    #[test]
    fn test_format_tag_semantic_release_compat() {
        // semantic-release style: v{version} for root, {name}@{version} for packages
        let config = Config {
            tag_format: "v{version}".into(),
            tag_format_package: "{name}@{version}".into(),
            ..Config::default()
        };
        let v = semver::Version::new(1, 5, 0);
        assert_eq!(config.format_tag("root", &v, true), "v1.5.0");
        assert_eq!(config.format_tag("@scope/pkg", &v, false), "@scope/pkg@1.5.0");
    }

    #[test]
    fn test_tag_match_regex() {
        let config = Config::default();

        let re = config.tag_match_regex("my-app", true).unwrap();
        assert!(re.is_match("v1.2.3"));
        assert!(!re.is_match("my-app/v1.2.3"));
        let caps = re.captures("v1.2.3").unwrap();
        assert_eq!(&caps["version"], "1.2.3");

        let re = config.tag_match_regex("@acme/core", false).unwrap();
        assert!(re.is_match("@acme/core/v1.2.3"));
        assert!(!re.is_match("v1.2.3"));
        let caps = re.captures("@acme/core/v2.0.0-beta.1").unwrap();
        assert_eq!(&caps["version"], "2.0.0-beta.1");
    }

    #[test]
    fn test_tag_match_regex_custom() {
        let config = Config {
            tag_format: "{name}-v{version}".into(),
            tag_format_package: "{name}@{version}".into(),
            ..Config::default()
        };
        let re = config.tag_match_regex("my-app", true).unwrap();
        assert!(re.is_match("my-app-v3.0.0"));
        let caps = re.captures("my-app-v3.0.0").unwrap();
        assert_eq!(&caps["version"], "3.0.0");

        let re = config.tag_match_regex("@acme/lib", false).unwrap();
        assert!(re.is_match("@acme/lib@1.0.0"));
    }

    #[test]
    fn test_parse_yaml_simple_branches() {
        let yaml = r#"
branches:
  - main
  - develop
plugins:
  - name: changelog
"#;
        let config: Config = serde_saphyr::from_str(yaml).unwrap();
        assert_eq!(config.branches[0].name(), "main");
        assert_eq!(config.branches[1].name(), "develop");
        assert!(config.branches[0].resolve_prerelease("main").is_none());
    }

    #[test]
    fn test_parse_yaml_rich_branches() {
        let yaml = r#"
branches:
  - main
  - name: beta
    prerelease: beta
  - name: next
    prerelease: next
  - name: "test-*"
    prerelease: true
  - name: "1.x"
    maintenance: true
"#;
        let config: Config = serde_saphyr::from_str(yaml).unwrap();
        assert_eq!(config.branches.len(), 5);
        assert_eq!(config.branches[0].name(), "main");
        assert!(config.branches[0].resolve_prerelease("main").is_none());

        assert_eq!(config.branches[1].name(), "beta");
        assert_eq!(config.branches[1].resolve_prerelease("beta").as_deref(), Some("beta"));

        assert_eq!(config.branches[2].name(), "next");
        assert_eq!(config.branches[2].resolve_prerelease("next").as_deref(), Some("next"));

        // `prerelease: true` uses the actual branch name as channel
        assert_eq!(config.branches[3].name(), "test-*");
        assert_eq!(
            config.branches[3].resolve_prerelease("test-hello").as_deref(),
            Some("test-hello")
        );

        assert_eq!(config.branches[4].name(), "1.x");
        assert!(config.branches[4].is_maintenance());
    }

    #[test]
    fn test_prerelease_true_never_produces_literal_true() {
        let yaml = r#"
branches:
  - name: "feature-*"
    prerelease: true
"#;
        let config: Config = serde_saphyr::from_str(yaml).unwrap();
        let branch = &config.branches[0];

        let channel = branch.resolve_prerelease("feature-abc").unwrap();
        assert_eq!(channel, "feature-abc");
        assert_ne!(channel, "true");
    }

    #[test]
    fn test_prerelease_false_is_stable() {
        let yaml = r#"
branches:
  - name: staging
    prerelease: false
"#;
        let config: Config = serde_saphyr::from_str(yaml).unwrap();
        assert!(config.branches[0].resolve_prerelease("staging").is_none());
    }

    #[test]
    fn test_glob_match() {
        // Exact
        assert!(glob_match("main", "main"));
        assert!(!glob_match("main", "master"));

        // Wildcard *
        assert!(glob_match("*.x", "2.x"));
        assert!(glob_match("*.x", "15.x"));
        assert!(glob_match("test-*", "test-foo"));
        assert!(glob_match("test-*", "test-tsmain-1460"));
        assert!(!glob_match("test-*", "dev-foo"));

        // Scoped packages
        assert!(glob_match("@acme/*", "@acme/core"));
        assert!(glob_match("@acme/*", "@acme/utils"));
        assert!(!glob_match("@acme/*", "@other/core"));

        // Alternation
        assert!(glob_match("{@acme/*,@tools/*}", "@acme/core"));
        assert!(glob_match("{@acme/*,@tools/*}", "@tools/cli"));
        assert!(!glob_match("{@acme/*,@tools/*}", "@other/lib"));

        // Single char ?
        assert!(glob_match("pkg-?", "pkg-a"));
        assert!(!glob_match("pkg-?", "pkg-ab"));
    }
}
