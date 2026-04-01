mod common;

use common::{git, super_release_bin};
use predicates::prelude::*;
use std::fs;
use std::process;
use tempfile::TempDir;

#[test]
fn test_prerelease_branch_beta() {
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

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    git(root, &["tag", "-a", "v1.0.0", "-m", "v1.0.0"]);

    // Create and switch to beta branch
    git(root, &["checkout", "-b", "beta"]);

    fs::write(root.join("index.js"), "// beta feature").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat: upcoming feature"]);

    // Config with beta prerelease branch
    fs::write(
        root.join(".release.yaml"),
        r#"
branches:
  - main
  - name: beta
    prerelease: beta
steps:
  - name: changelog
"#,
    )
    .unwrap();

    super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("1.1.0-beta.1"))
        .stdout(predicate::str::contains("prerelease: beta"));
}

#[test]
fn test_prerelease_increment() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    git(root, &["init", "-b", "beta"]);
    git(root, &["config", "user.email", "test@test.com"]);
    git(root, &["config", "user.name", "Test"]);

    fs::write(
        root.join("package.json"),
        r#"{"name": "my-app", "version": "2.0.0"}"#,
    )
    .unwrap();
    fs::write(root.join("index.js"), "// v2 beta").unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    // Already on beta.3
    git(root, &["tag", "-a", "v2.0.0-beta.3", "-m", "v2.0.0-beta.3"]);

    fs::write(root.join("index.js"), "// another fix").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix: beta fix"]);

    fs::write(
        root.join(".release.yaml"),
        r#"
branches:
  - name: beta
    prerelease: beta
steps:
"#,
    )
    .unwrap();

    super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("2.0.0-beta.4"));
}

#[test]
fn test_prerelease_branch_pattern_uses_branch_name() {
    // `prerelease: true` with pattern `test-*` should use the actual branch name as channel
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.email", "test@test.com"]);
    git(root, &["config", "user.name", "Test"]);

    fs::write(
        root.join("package.json"),
        r#"{"name": "my-app", "version": "10.235.0"}"#,
    )
    .unwrap();
    fs::write(root.join("index.js"), "// stable").unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    git(root, &["tag", "-a", "v10.235.0", "-m", "v10.235.0"]);

    // Create feature/test branch
    git(root, &["checkout", "-b", "test-hello"]);

    fs::write(root.join("index.js"), "// test feature").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat: test feature"]);

    fs::write(
        root.join(".release.yaml"),
        r#"
branches:
  - main
  - name: "test-*"
    prerelease: true
steps:
"#,
    )
    .unwrap();

    super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("10.236.0-test-hello.1"))
        .stdout(predicate::str::contains("prerelease: test-hello"));
}

#[test]
fn test_stable_branch_ignores_prerelease_tags() {
    // On main, prerelease tags from other branches should not affect version calculation
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

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    git(root, &["tag", "-a", "v1.0.0", "-m", "v1.0.0"]);

    // Simulate prerelease tags from another branch (as if beta branch released)
    git(root, &["tag", "-a", "v1.1.0-beta.1", "-m", "beta"]);
    git(root, &["tag", "-a", "v1.1.0-beta.2", "-m", "beta"]);
    git(root, &["tag", "-a", "v2.0.0-test-foo.1", "-m", "test"]);

    fs::write(root.join("index.js"), "// v1 fix").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix: bug fix"]);

    fs::write(
        root.join(".release.yaml"),
        r#"
branches:
  - main
  - name: beta
    prerelease: beta
  - name: "test-*"
    prerelease: true
steps:
"#,
    )
    .unwrap();

    let output = super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Should bump from v1.0.0 (the stable tag), NOT from v2.0.0-test-foo.1
    assert!(
        stdout.contains("1.0.1"),
        "Expected 1.0.1 (patch from v1.0.0) but got:\n{}",
        stdout
    );
    // Should NOT pick up the prerelease versions
    assert!(
        !stdout.contains("beta"),
        "Should not reference beta tags:\n{}",
        stdout
    );
}

#[test]
fn test_prerelease_branch_increment_with_pattern() {
    // On test-foo branch with existing test-foo prerelease tags, should increment
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    git(root, &["init", "-b", "test-foo"]);
    git(root, &["config", "user.email", "test@test.com"]);
    git(root, &["config", "user.name", "Test"]);

    fs::write(
        root.join("package.json"),
        r#"{"name": "my-app", "version": "1.0.0"}"#,
    )
    .unwrap();
    fs::write(root.join("index.js"), "// test").unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    git(root, &["tag", "-a", "v1.0.0", "-m", "v1.0.0"]);
    git(
        root,
        &["tag", "-a", "v1.1.0-test-foo.2", "-m", "prerelease"],
    );

    fs::write(root.join("index.js"), "// more work").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat: more work"]);

    fs::write(
        root.join(".release.yaml"),
        r#"
branches:
  - main
  - name: "test-*"
    prerelease: true
steps:
"#,
    )
    .unwrap();

    super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("1.1.0-test-foo.3"));
}

/// Regression test: on a prerelease branch, commits merged from another branch
/// that already have a stable release tag must still be picked up if they are
/// new to this prerelease channel.
///
/// Scenario:
///   main:       init ── feat ── [v1.1.0] ── fix ── [v1.1.1]
///   beta:       (branch from init) ── [v1.1.0-beta.1] ── merge main ── ???
///
/// The fix commit (v1.1.1 on main) is already covered by the stable tag, but
/// it is NEW to the beta branch. super-release should detect it and produce
/// v1.1.2-beta.1.
#[test]
fn test_prerelease_picks_up_commits_merged_from_stable() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.email", "test@test.com"]);
    git(root, &["config", "user.name", "Test"]);

    // Initial setup on main
    fs::write(
        root.join("package.json"),
        r#"{"name": "my-app", "version": "1.0.0"}"#,
    )
    .unwrap();
    fs::write(root.join("index.js"), "// v1").unwrap();
    fs::write(
        root.join(".release.yaml"),
        r#"
branches:
  - main
  - name: beta
    prerelease: beta
steps:
"#,
    )
    .unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    git(root, &["tag", "-a", "v1.0.0", "-m", "v1.0.0"]);

    // Feature commit on main → v1.1.0
    fs::write(root.join("index.js"), "// v1.1").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat: new feature"]);
    git(root, &["tag", "-a", "v1.1.0", "-m", "v1.1.0"]);

    // Create beta branch from current main (at v1.1.0)
    git(root, &["checkout", "-b", "beta"]);

    // Make a beta-only commit so we get a prerelease tag
    fs::write(root.join("index.js"), "// beta").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat: beta feature"]);
    git(root, &["tag", "-a", "v1.2.0-beta.1", "-m", "v1.2.0-beta.1"]);

    // Switch back to main and make a fix commit → v1.1.1
    git(root, &["checkout", "main"]);
    fs::write(root.join("index.js"), "// v1.1.1 fix").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix: important bugfix"]);
    git(root, &["tag", "-a", "v1.1.1", "-m", "v1.1.1"]);

    // Switch to beta and merge main (bringing in the fix commit)
    git(root, &["checkout", "beta"]);
    // Resolve merge conflict by taking main's version
    let merge = process::Command::new("git")
        .args(["merge", "main", "-m", "Merge main into beta"])
        .current_dir(root)
        .output()
        .unwrap();
    if !merge.status.success() {
        // Conflict expected — resolve by accepting main's changes
        fs::write(root.join("index.js"), "// v1.1.1 fix + beta").unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-m", "Merge main into beta"]);
    }

    // Now super-release should detect the fix commit as new to beta
    let output = super_release_bin()
        .arg("--dry-run")
        .arg("-v")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "Failed:\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );
    assert!(
        stdout.contains("beta"),
        "Should produce a prerelease:\n{}",
        stdout
    );
    assert!(
        !stdout.contains("No releases needed"),
        "Should detect the merged fix commit as new:\n{}",
        stdout
    );
    // Should bump beyond v1.2.0-beta.1 (the existing prerelease)
    assert!(
        stdout.contains("beta."),
        "Should produce a beta prerelease version:\n{}",
        stdout
    );
}
