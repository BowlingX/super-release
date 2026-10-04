mod common;

use common::{git, super_release_bin};
use std::fs;
use std::path::Path;
use tempfile::TempDir;

fn setup_repo(prepare_cmd: &str) -> TempDir {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.email", "test@test.com"]);
    git(root, &["config", "user.name", "Test"]);

    fs::write(
        root.join("package.json"),
        r#"{"name": "my-app", "version": "1.0.0"}"#,
    )
    .unwrap();
    fs::write(root.join("index.js"), "// v1").unwrap();
    fs::write(
        root.join(".release.yaml"),
        format!(
            r#"
branches: [main]
steps:
  - name: exec
    options:
      prepare_cmd: "{prepare_cmd}"
"#
        ),
    )
    .unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    git(root, &["tag", "-a", "v1.0.0", "-m", "v1.0.0"]);

    fs::write(root.join("index.js"), "// v1.1").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat: new feature"]);
    dir
}

fn run(root: &Path, env: &[(&str, &str)], args: &[&str]) -> (bool, String) {
    let mut cmd = super_release_bin();
    for (k, v) in env {
        cmd.env(k, v);
    }
    let output = cmd
        .args(args)
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();
    (
        output.status.success(),
        String::from_utf8(output.stdout).unwrap(),
    )
}

fn assert_balanced_github_groups(stdout: &str) {
    let mut open = false;
    for line in stdout.lines() {
        if line.starts_with("::group::") {
            assert!(!open, "nested group at {line:?} in:\n{stdout}");
            open = true;
        } else if line == "::endgroup::" {
            assert!(open, "endgroup without group in:\n{stdout}");
            open = false;
        }
    }
    assert!(!open, "unclosed group in:\n{stdout}");
}

/// Title of the GitHub group the first line containing `needle` was printed in.
fn enclosing_group<'a>(stdout: &'a str, needle: &str) -> Option<&'a str> {
    let mut current = None;
    for line in stdout.lines() {
        if let Some(title) = line.strip_prefix("::group::") {
            current = Some(title);
        } else if line == "::endgroup::" {
            current = None;
        } else if line.contains(needle) {
            return current;
        }
    }
    panic!("{needle:?} not found in:\n{stdout}");
}

#[test]
fn test_github_actions_groups_sections() {
    let dir = setup_repo("echo releasing {name}");
    let root = dir.path();

    let (ok, stdout) = run(root, &[("GITHUB_ACTIONS", "true")], &["--dry-run"]);
    assert!(ok, "{stdout}");
    assert_balanced_github_groups(&stdout);

    for title in [
        "Discovered 1 package(s):",
        "Bumping package versions",
        "Running step: exec",
        "Finalizing git commit and tags",
    ] {
        assert!(
            stdout.contains(&format!("::group::{title}\n")),
            "missing group {title:?} in:\n{stdout}"
        );
    }
    assert_eq!(
        enclosing_group(&stdout, "[exec:prepare] Would run"),
        Some("Running step: exec")
    );
    assert_eq!(enclosing_group(&stdout, "Release plan"), None);
    assert_eq!(enclosing_group(&stdout, "Dry run complete"), None);
    assert!(!stdout.contains(">> Running step"), "{stdout}");
}

#[test]
fn test_gitlab_sections_have_matching_ids() {
    let dir = setup_repo("echo releasing {name}");
    let root = dir.path();

    let (ok, stdout) = run(root, &[("GITLAB_CI", "true")], &["--dry-run"]);
    assert!(ok, "{stdout}");

    let ids = |marker: &str| -> Vec<String> {
        stdout
            .split(marker)
            .skip(1)
            .map(|rest| {
                let id = rest.split_once(':').unwrap().1;
                id.split(['[', '\r']).next().unwrap().to_string()
            })
            .collect()
    };
    let starts = ids("section_start:");
    assert!(!starts.is_empty(), "{stdout}");
    assert_eq!(starts, ids("section_end:"), "{stdout}");
    assert!(
        stdout.contains("[collapsed=true]\r\x1b[0KRunning step: exec\n"),
        "{stdout}"
    );
}

#[test]
fn test_azure_pipelines_groups_sections() {
    let dir = setup_repo("echo releasing {name}");
    let root = dir.path();

    let (ok, stdout) = run(root, &[("TF_BUILD", "True")], &["--dry-run"]);
    assert!(ok, "{stdout}");
    assert!(stdout.contains("##[group]Running step: exec\n"), "{stdout}");
    assert_eq!(
        stdout.matches("##[group]").count(),
        stdout.matches("##[endgroup]").count(),
        "{stdout}"
    );
}

#[test]
fn test_no_groups_outside_ci() {
    let dir = setup_repo("echo releasing {name}");
    let root = dir.path();

    let (ok, stdout) = run(root, &[], &["--dry-run"]);
    assert!(ok, "{stdout}");
    assert!(stdout.contains(">> Running step: exec"), "{stdout}");
    assert!(stdout.contains(">> Discovered 1 package(s):"), "{stdout}");
    assert!(!stdout.contains("::group::"), "{stdout}");
    assert!(!stdout.contains("section_start"), "{stdout}");
}

#[test]
fn test_show_next_version_has_no_groups() {
    let dir = setup_repo("echo releasing {name}");
    let root = dir.path();

    let (ok, stdout) = run(
        root,
        &[("GITHUB_ACTIONS", "true")],
        &["--show-next-version"],
    );
    assert!(ok, "{stdout}");
    assert_eq!(stdout.trim(), "1.1.0");
}

#[test]
fn test_group_closes_when_step_fails() {
    let dir = setup_repo("echo about to fail && exit 1");
    let root = dir.path();

    let (ok, stdout) = run(root, &[("GITHUB_ACTIONS", "true")], &[]);
    assert!(!ok, "{stdout}");
    assert_balanced_github_groups(&stdout);
    assert_eq!(
        enclosing_group(&stdout, "about to fail"),
        Some("Running step: exec")
    );
    assert!(stdout.trim_end().ends_with("::endgroup::"), "{stdout}");
}
