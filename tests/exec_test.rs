use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::Path;
use std::process;
use tempfile::TempDir;

fn git(dir: &Path, args: &[&str]) {
    let output = process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    if !output.status.success() {
        panic!(
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn super_release_bin() -> Command {
    Command::cargo_bin("super-release").unwrap()
}

fn setup_repo(root: &Path) {
    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.email", "test@test.com"]);
    git(root, &["config", "user.name", "Test"]);

    fs::write(
        root.join("package.json"),
        r#"{"name": "my-app", "version": "1.0.0"}"#,
    )
    .unwrap();
    fs::write(root.join("index.js"), "// v1").unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    git(root, &["tag", "-a", "v1.0.0", "-m", "v1.0.0"]);

    fs::write(root.join("index.js"), "// v1.1").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat: new feature"]);
}

fn setup_monorepo(root: &Path) {
    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.email", "test@test.com"]);
    git(root, &["config", "user.name", "Test"]);

    fs::write(
        root.join("package.json"),
        r#"{"name": "mono-root", "version": "1.0.0", "private": true}"#,
    )
    .unwrap();

    fs::create_dir_all(root.join("packages/core/src")).unwrap();
    fs::write(
        root.join("packages/core/package.json"),
        r#"{"name": "@test/core", "version": "1.0.0"}"#,
    )
    .unwrap();
    fs::write(root.join("packages/core/src/index.ts"), "// v1").unwrap();

    fs::create_dir_all(root.join("packages/cli/src")).unwrap();
    fs::write(
        root.join("packages/cli/package.json"),
        r#"{"name": "@test/cli", "version": "1.0.0"}"#,
    )
    .unwrap();
    fs::write(root.join("packages/cli/src/index.ts"), "// v1").unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    git(root, &["tag", "-a", "@test/core/v1.0.0", "-m", "v1"]);
    git(root, &["tag", "-a", "@test/cli/v1.0.0", "-m", "v1"]);

    fs::write(root.join("packages/core/src/index.ts"), "// v1.1").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat(core): add feature"]);

    fs::write(root.join("packages/cli/src/index.ts"), "// v1.1").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix(cli): fix bug"]);
}

#[test]
fn test_exec_prepare_dry_run() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    setup_repo(root);

    fs::write(
        root.join(".release.yaml"),
        r#"
branches: [main]
plugins:
  - name: exec
    options:
      prepare_cmd: "echo releasing {name} v{version}"
"#,
    )
    .unwrap();

    super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "[exec:prepare] Would run: echo releasing my-app v1.1.0",
        ));
}

#[test]
fn test_exec_runs_command() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    setup_repo(root);

    // Write a marker file to prove the command ran
    fs::write(
        root.join(".release.yaml"),
        r#"
branches: [main]
plugins:
  - name: exec
    options:
      prepare_cmd: "echo {version} > VERSION.txt"
"#,
    )
    .unwrap();

    super_release_bin()
        .arg("-C")
        .arg(root.to_str().unwrap())
        .assert()
        .success();

    let version = fs::read_to_string(root.join("VERSION.txt")).unwrap();
    assert_eq!(version.trim(), "1.1.0");
}

#[test]
fn test_exec_package_filter() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    setup_monorepo(root);

    // Only run exec for @test/core, not @test/cli
    fs::write(
        root.join(".release.yaml"),
        r#"
branches: [main]
exclude: [mono-root]
plugins:
  - name: exec
    options:
      packages: ["@test/core"]
      prepare_cmd: "echo {name}={version} >> releases.txt"
"#,
    )
    .unwrap();

    super_release_bin()
        .arg("-C")
        .arg(root.to_str().unwrap())
        .assert()
        .success();

    let releases = fs::read_to_string(root.join("releases.txt")).unwrap();
    assert!(
        releases.contains("@test/core=1.1.0"),
        "Should include core: {}",
        releases
    );
    assert!(
        !releases.contains("@test/cli"),
        "Should NOT include cli: {}",
        releases
    );
}

#[test]
fn test_exec_multiple_instances() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    setup_monorepo(root);

    // Two exec blocks with different filters and commands
    fs::write(
        root.join(".release.yaml"),
        r#"
branches: [main]
exclude: [mono-root]
plugins:
  - name: exec
    options:
      packages: ["@test/core"]
      prepare_cmd: "echo core={version} >> output.txt"
  - name: exec
    options:
      packages: ["@test/cli"]
      prepare_cmd: "echo cli={version} >> output.txt"
"#,
    )
    .unwrap();

    super_release_bin()
        .arg("-C")
        .arg(root.to_str().unwrap())
        .assert()
        .success();

    let output = fs::read_to_string(root.join("output.txt")).unwrap();
    assert!(output.contains("core=1.1.0"), "Missing core: {}", output);
    assert!(output.contains("cli=1.0.1"), "Missing cli: {}", output);
}

#[test]
fn test_exec_glob_filter() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    setup_monorepo(root);

    // Glob pattern to match all @test/* packages
    fs::write(
        root.join(".release.yaml"),
        r#"
branches: [main]
exclude: [mono-root]
plugins:
  - name: exec
    options:
      packages: ["@test/*"]
      prepare_cmd: "echo {name} >> matched.txt"
"#,
    )
    .unwrap();

    super_release_bin()
        .arg("-C")
        .arg(root.to_str().unwrap())
        .assert()
        .success();

    let matched = fs::read_to_string(root.join("matched.txt")).unwrap();
    assert!(matched.contains("@test/core"), "Missing core: {}", matched);
    assert!(matched.contains("@test/cli"), "Missing cli: {}", matched);
}

#[test]
fn test_exec_publish_phase() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    setup_repo(root);

    fs::write(
        root.join(".release.yaml"),
        r#"
branches: [main]
plugins:
  - name: exec
    options:
      publish_cmd: "echo published {name}@{version} > published.txt"
"#,
    )
    .unwrap();

    super_release_bin()
        .arg("-C")
        .arg(root.to_str().unwrap())
        .assert()
        .success();

    let published = fs::read_to_string(root.join("published.txt")).unwrap();
    assert_eq!(published.trim(), "published my-app@1.1.0");
}

#[test]
fn test_exec_cargo_toml_version_bump() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    setup_repo(root);

    // Simulate a Cargo.toml
    fs::write(
        root.join("Cargo.toml"),
        r#"[package]
name = "my-app"
version = "1.0.0"
edition = "2024"
"#,
    )
    .unwrap();

    fs::write(
        root.join(".release.yaml"),
        r#"
branches: [main]
plugins:
  - name: exec
    options:
      prepare_cmd: "sed -i'' -e 's/^version = .*/version = \"{version}\"/' Cargo.toml"
"#,
    )
    .unwrap();

    super_release_bin()
        .arg("-C")
        .arg(root.to_str().unwrap())
        .assert()
        .success();

    let cargo_toml = fs::read_to_string(root.join("Cargo.toml")).unwrap();
    assert!(
        cargo_toml.contains("version = \"1.1.0\""),
        "Cargo.toml should have bumped version: {}",
        cargo_toml
    );
}
