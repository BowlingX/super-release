mod branch;

pub use branch::{BranchConfig, BranchContext};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Top-level configuration for super-release.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
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

pub fn resolve_branch_context(
    repo: &git2::Repository,
    config: &Config,
) -> Result<Option<BranchContext>> {
    branch::resolve_branch_context(repo, &config.branches)
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
    fn test_parse_yaml_config() {
        let yaml = r#"
branches:
  - main
  - name: beta
    prerelease: beta
plugins:
  - name: changelog
"#;
        let config: Config = serde_saphyr::from_str(yaml).unwrap();
        assert_eq!(config.branches.len(), 2);
        assert_eq!(config.branches[0].name(), "main");
        assert_eq!(config.branches[1].name(), "beta");
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
