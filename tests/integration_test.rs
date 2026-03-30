use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::Path;
use std::process;
use tempfile::TempDir;

/// Helper to create a git repo with a monorepo structure and conventional commits.
fn setup_monorepo() -> TempDir {
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    // Init git repo
    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.email", "test@test.com"]);
    git(root, &["config", "user.name", "Test"]);

    // Create root package.json
    fs::write(
        root.join("package.json"),
        r#"{"name": "monorepo-root", "version": "1.0.0", "private": true}"#,
    )
    .unwrap();

    // Create packages/core
    fs::create_dir_all(root.join("packages/core/src")).unwrap();
    fs::write(
        root.join("packages/core/package.json"),
        r#"{"name": "@myorg/core", "version": "1.0.0"}"#,
    )
    .unwrap();
    fs::write(root.join("packages/core/src/index.ts"), "export const x = 1;").unwrap();

    // Create packages/utils (depends on core)
    fs::create_dir_all(root.join("packages/utils/src")).unwrap();
    fs::write(
        root.join("packages/utils/package.json"),
        r#"{"name": "@myorg/utils", "version": "1.0.0", "dependencies": {"@myorg/core": "^1.0.0"}}"#,
    )
    .unwrap();
    fs::write(root.join("packages/utils/src/index.ts"), "export const y = 2;").unwrap();

    // Initial commit
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: initial commit"]);

    // Tag initial versions
    git(root, &["tag", "-a", "@myorg/core@1.0.0", "-m", "Release @myorg/core@1.0.0"]);
    git(root, &["tag", "-a", "@myorg/utils@1.0.0", "-m", "Release @myorg/utils@1.0.0"]);

    // Add a feature commit to core
    fs::write(root.join("packages/core/src/index.ts"), "export const x = 1;\nexport const z = 3;").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat(core): add new export z"]);

    // Add a fix commit to utils
    fs::write(root.join("packages/utils/src/index.ts"), "export const y = 2;\n// fixed").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix(utils): fix import issue"]);

    dir
}

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

#[test]
fn test_help_flag() {
    super_release_bin()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("fast semantic-release alternative"))
        .stdout(predicate::str::contains("--dry-run"));
}

#[test]
fn test_version_flag() {
    super_release_bin()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("super-release"));
}

#[test]
fn test_dry_run_monorepo() {
    let dir = setup_monorepo();

    // Create a config file
    fs::write(
        dir.path().join(".release.yaml"),
        r#"
branches:
  - main
plugins:
  - name: changelog
  - name: npm
  - name: git-tag
"#,
    )
    .unwrap();

    super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(dir.path().to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("dry run"))
        .stdout(predicate::str::contains("@myorg/core"))
        .stdout(predicate::str::contains("@myorg/utils"))
        .stdout(predicate::str::contains("1.1.0")) // core: feat -> minor bump
        .stdout(predicate::str::contains("1.0.1")); // utils: fix -> patch bump
}

#[test]
fn test_dry_run_no_changes() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.email", "test@test.com"]);
    git(root, &["config", "user.name", "Test"]);

    fs::write(
        root.join("package.json"),
        r#"{"name": "my-pkg", "version": "1.0.0"}"#,
    )
    .unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: initial"]);
    git(root, &["tag", "-a", "my-pkg@1.0.0", "-m", "Release"]);

    super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("No releases needed"));
}

#[test]
fn test_package_filter() {
    let dir = setup_monorepo();

    super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(dir.path().to_str().unwrap())
        .arg("-p")
        .arg("core")
        .assert()
        .success()
        .stdout(predicate::str::contains("@myorg/core"))
        .stdout(predicate::str::contains("1.1.0"));
}

#[test]
fn test_breaking_change_major_bump() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.email", "test@test.com"]);
    git(root, &["config", "user.name", "Test"]);

    fs::write(
        root.join("package.json"),
        r#"{"name": "my-lib", "version": "2.0.0"}"#,
    )
    .unwrap();
    fs::write(root.join("index.js"), "module.exports = {}").unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    git(root, &["tag", "-a", "my-lib@2.0.0", "-m", "v2"]);

    // Breaking change
    fs::write(root.join("index.js"), "export default {}").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat!: switch to ESM\n\nBREAKING CHANGE: CommonJS no longer supported"]);

    super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("3.0.0"))
        .stdout(predicate::str::contains("major"));
}
