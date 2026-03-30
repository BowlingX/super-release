use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Top-level configuration for super-release.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Branch patterns to release from (default: ["main", "master"])
    #[serde(default = "default_branches")]
    pub branches: Vec<String>,

    /// Tag format. Use `{name}` and `{version}` placeholders.
    /// Default: "{name}@{version}"
    #[serde(default = "default_tag_format")]
    pub tag_format: String,

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
    pub options: serde_yaml::Value,
}

fn default_branches() -> Vec<String> {
    vec!["main".into(), "master".into()]
}

fn default_tag_format() -> String {
    "{name}@{version}".into()
}

fn default_plugins() -> Vec<PluginConfig> {
    vec![
        PluginConfig {
            name: "changelog".into(),
            options: serde_yaml::Value::Null,
        },
        PluginConfig {
            name: "npm".into(),
            options: serde_yaml::Value::Null,
        },
        PluginConfig {
            name: "git-tag".into(),
            options: serde_yaml::Value::Null,
        },
    ]
}

impl Default for Config {
    fn default() -> Self {
        Config {
            branches: default_branches(),
            tag_format: default_tag_format(),
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
        if path.exists() {
            return load_config_from_path(&path);
        }
    }

    Ok(Config::default())
}

fn load_config_from_path(path: &Path) -> Result<Config> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("reading config file: {}", path.display()))?;
    let config: Config = serde_yaml::from_str(&content)
        .with_context(|| format!("parsing config file: {}", path.display()))?;
    Ok(config)
}

/// Resolve the repository root from a starting path.
pub fn find_repo_root(start: &Path) -> Result<PathBuf> {
    let repo = git2::Repository::discover(start)?;
    let workdir = repo
        .workdir()
        .context("Bare repositories are not supported")?;
    Ok(workdir.to_path_buf())
}

impl Config {
    /// Format a tag name using the configured tag_format.
    pub fn format_tag(&self, package_name: &str, version: &semver::Version) -> String {
        self.tag_format
            .replace("{name}", package_name)
            .replace("{version}", &version.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.branches, vec!["main", "master"]);
        assert_eq!(config.tag_format, "{name}@{version}");
        assert_eq!(config.plugins.len(), 3);
        assert_eq!(config.plugins[0].name, "changelog");
        assert_eq!(config.plugins[1].name, "npm");
        assert_eq!(config.plugins[2].name, "git-tag");
    }

    #[test]
    fn test_format_tag() {
        let config = Config::default();
        let v = semver::Version::new(1, 2, 3);
        assert_eq!(config.format_tag("@myorg/core", &v), "@myorg/core@1.2.3");
    }

    #[test]
    fn test_parse_yaml_config() {
        let yaml = r#"
branches:
  - main
  - develop
tag_format: "v{version}"
plugins:
  - name: changelog
    options:
      file: CHANGES.md
  - name: npm
  - name: git-tag
"#;
        let config: Config = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.branches, vec!["main", "develop"]);
        assert_eq!(config.tag_format, "v{version}");
        assert_eq!(config.plugins.len(), 3);
    }
}
