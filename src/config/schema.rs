use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

/// Bundled JSON schema for config validation.
static SCHEMA_JSON: &str = include_str!("../../schema.json");

/// Supported config file format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigFormat {
    Yaml,
    Json,
}

/// Known config file candidates in priority order.
const CANDIDATES: &[(&str, ConfigFormat)] = &[
    (".release.yaml", ConfigFormat::Yaml),
    (".release.yml", ConfigFormat::Yaml),
    (".release.json", ConfigFormat::Json),
    (".release.jsonc", ConfigFormat::Json),
];

/// Detect the format from a file extension.
fn detect_format(path: &Path) -> ConfigFormat {
    match path.extension().and_then(|e| e.to_str()) {
        Some("json" | "jsonc") => ConfigFormat::Json,
        _ => ConfigFormat::Yaml,
    }
}

/// Parse JSONC (JSON with comments / trailing commas) into a serde_json::Value.
fn parse_jsonc(content: &str) -> Result<serde_json::Value> {
    jsonc_parser::parse_to_serde_value(content, &Default::default())
        .map_err(|e| anyhow::anyhow!("Failed to parse JSON: {}", e))
}

/// Parse raw config content into a serde_json::Value based on format.
pub fn parse_to_json_value(content: &str, format: ConfigFormat) -> Result<serde_json::Value> {
    match format {
        ConfigFormat::Yaml => {
            serde_saphyr::from_str(content).context("Failed to parse YAML config")
        }
        ConfigFormat::Json => parse_jsonc(content),
    }
}

/// Parse raw config content into a Config struct.
pub fn parse_config(content: &str, format: ConfigFormat) -> Result<super::Config> {
    match format {
        ConfigFormat::Yaml => {
            serde_saphyr::from_str(content).context("Failed to parse YAML config")
        }
        ConfigFormat::Json => {
            let value = parse_jsonc(content)?;
            serde_json::from_value(value).context("Failed to deserialize config")
        }
    }
}

/// Compiled schema validator, cached for reuse.
static SCHEMA_VALIDATOR: LazyLock<jsonschema::Validator> = LazyLock::new(|| {
    let schema: serde_json::Value =
        serde_json::from_str(SCHEMA_JSON).expect("bundled schema.json is invalid — this is a bug");
    jsonschema::validator_for(&schema).expect("schema compilation failed — this is a bug")
});

/// Validate raw config content against the bundled JSON schema.
/// Returns a list of validation error messages (empty = valid).
pub fn validate(content: &str, format: ConfigFormat) -> Vec<String> {
    let json_value = match parse_to_json_value(content, format) {
        Ok(v) => v,
        Err(e) => return vec![e.to_string()],
    };

    SCHEMA_VALIDATOR
        .iter_errors(&json_value)
        .map(|err| {
            let path = err.instance_path().to_string();
            if path.is_empty() {
                err.to_string()
            } else {
                format!("{}: {}", path, err)
            }
        })
        .collect()
}

/// Locate and read the config file. Returns the raw content, path, and format.
/// If `path` is a file, reads it directly. If it's a directory, searches for
/// `.release.yaml`, `.release.json`, or `.release.jsonc`.
/// Returns `None` if no config file was found (defaults will be used).
pub fn find_config(path: &Path) -> Result<Option<(String, PathBuf, ConfigFormat)>> {
    if path.is_file() {
        let content =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        return Ok(Some((content, path.to_path_buf(), detect_format(path))));
    }

    for &(candidate, format) in CANDIDATES {
        let file = path.join(candidate);
        match std::fs::read_to_string(&file) {
            Ok(content) => return Ok(Some((content, file, format))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(anyhow::anyhow!("reading {}: {}", file.display(), e)),
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_valid_yaml() {
        let yaml = "branches:\n  - main\nsteps:\n  - name: changelog\n";
        let errors = validate(yaml, ConfigFormat::Yaml);
        assert!(errors.is_empty(), "unexpected errors: {:?}", errors);
    }

    #[test]
    fn test_validate_invalid_yaml() {
        let yaml = "unknown_key: true\n";
        let errors = validate(yaml, ConfigFormat::Yaml);
        assert!(!errors.is_empty());
        assert!(errors[0].contains("unknown_key"));
    }

    #[test]
    fn test_validate_valid_json() {
        let json = r#"{"branches": ["main"], "steps": [{"name": "npm"}]}"#;
        let errors = validate(json, ConfigFormat::Json);
        assert!(errors.is_empty(), "unexpected errors: {:?}", errors);
    }

    #[test]
    fn test_validate_valid_jsonc() {
        let jsonc = r#"{
  // branches to release from
  "branches": ["main"],
  "steps": [{"name": "npm"}], // trailing comma is fine
}"#;
        let errors = validate(jsonc, ConfigFormat::Json);
        assert!(errors.is_empty(), "unexpected errors: {:?}", errors);
    }

    #[test]
    fn test_validate_invalid_json() {
        let json = r#"{"unknown_field": true}"#;
        let errors = validate(json, ConfigFormat::Json);
        assert!(!errors.is_empty());
        assert!(errors[0].contains("unknown_field"));
    }

    #[test]
    fn test_parse_config_json() {
        let json = r#"{"branches": ["main"], "steps": [{"name": "changelog"}]}"#;
        let config = parse_config(json, ConfigFormat::Json).unwrap();
        assert_eq!(config.branches.len(), 1);
        assert_eq!(config.steps.len(), 1);
        assert_eq!(config.steps[0].name, "changelog");
    }

    #[test]
    fn test_parse_config_jsonc_with_comments() {
        let jsonc = r#"{
  // Use only main branch
  "branches": ["main"],
  /* No steps needed */
  "steps": []
}"#;
        let config = parse_config(jsonc, ConfigFormat::Json).unwrap();
        assert_eq!(config.branches.len(), 1);
        assert!(config.steps.is_empty());
    }

    #[test]
    fn test_detect_format() {
        assert_eq!(detect_format(Path::new("foo.yaml")), ConfigFormat::Yaml);
        assert_eq!(detect_format(Path::new("foo.yml")), ConfigFormat::Yaml);
        assert_eq!(detect_format(Path::new("foo.json")), ConfigFormat::Json);
        assert_eq!(detect_format(Path::new("foo.jsonc")), ConfigFormat::Json);
        assert_eq!(detect_format(Path::new("foo")), ConfigFormat::Yaml);
    }
}
