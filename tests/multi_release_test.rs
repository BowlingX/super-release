mod common;

use common::{git, super_release_bin};
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

#[test]
fn test_next_branch_releases_with_channel() {
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
        r#"
branches:
  - main
  - name: next
    channel: next
steps: []
"#,
    )
    .unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    git(root, &["tag", "-a", "v1.0.0", "-m", "v1.0.0"]);

    // Create next branch and add a feature
    git(root, &["checkout", "-b", "next"]);
    fs::write(root.join("index.js"), "// next feature").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat: upcoming feature"]);

    let output = super_release_bin()
        .arg("--dry-run")
        .arg("-v")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "Should succeed:\nstdout: {}\nstderr: {}",
        stdout,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("1.1.0"),
        "Should produce v1.1.0:\n{}",
        stdout
    );
    assert!(
        stdout.contains("channel: next"),
        "Should show channel: next:\n{}",
        stdout
    );
}

#[test]
fn test_collision_between_release_branches() {
    // next released v1.1.0, main tries to release v1.1.0 → ERROR
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
        r#"
branches:
  - main
  - name: next
    channel: next
steps: []
"#,
    )
    .unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    git(root, &["tag", "-a", "v1.0.0", "-m", "v1.0.0"]);

    // Create next branch and release v1.1.0 there
    git(root, &["checkout", "-b", "next"]);
    fs::write(root.join("index.js"), "// next feature").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat: next feature"]);
    git(root, &["tag", "-a", "v1.1.0", "-m", "v1.1.0"]);

    // Back on main, add a different feat commit (would also produce v1.1.0)
    git(root, &["checkout", "main"]);
    fs::write(root.join("index.js"), "// main feature").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat: main feature"]);

    let output = super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "Should fail — v1.1.0 already exists on next branch"
    );
    assert!(
        stderr.contains("already exists as a tag"),
        "Should report collision:\n{}",
        stderr
    );
}

#[test]
fn test_next_major_branch() {
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
        r#"
branches:
  - main
  - name: next-major
    channel: next-major
steps: []
"#,
    )
    .unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    git(root, &["tag", "-a", "v1.0.0", "-m", "v1.0.0"]);

    git(root, &["checkout", "-b", "next-major"]);
    fs::write(root.join("index.js"), "// breaking").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat!: breaking redesign"]);

    let output = super_release_bin()
        .arg("--dry-run")
        .arg("-v")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "Should succeed:\nstdout: {}\nstderr: {}",
        stdout,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("2.0.0"),
        "Should produce v2.0.0:\n{}",
        stdout
    );
    assert!(
        stdout.contains("channel: next-major"),
        "Should show channel:\n{}",
        stdout
    );
}

#[test]
fn test_channel_sets_npm_dist_tag() {
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
        r#"
branches:
  - main
  - name: next
    channel: next
steps:
  - name: npm
"#,
    )
    .unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    git(root, &["tag", "-a", "v1.0.0", "-m", "v1.0.0"]);

    git(root, &["checkout", "-b", "next"]);
    fs::write(root.join("index.js"), "// next").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat: next feature"]);

    let output = super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "Should succeed:\nstdout: {}\nstderr: {}",
        stdout,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("--tag next"),
        "npm publish should use --tag next:\n{}",
        stdout
    );
}

#[test]
fn test_no_collision_when_versions_differ() {
    // next is at v1.2.0, main produces v1.1.0 — no collision
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
        r#"
branches:
  - main
  - name: next
    channel: next
steps: []
"#,
    )
    .unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    git(root, &["tag", "-a", "v1.0.0", "-m", "v1.0.0"]);

    // next releases v1.1.0 and v1.2.0
    git(root, &["checkout", "-b", "next"]);
    fs::write(root.join("index.js"), "// feat 1").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat: feature one"]);
    git(root, &["tag", "-a", "v1.1.0", "-m", "v1.1.0"]);

    fs::write(root.join("index.js"), "// feat 2").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat: feature two"]);
    git(root, &["tag", "-a", "v1.2.0", "-m", "v1.2.0"]);

    // Back on main, add a fix (would produce v1.0.1 — no collision)
    git(root, &["checkout", "main"]);
    fs::write(root.join("index.js"), "// fix").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix: patch on main"]);

    let output = super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "Should succeed — v1.0.1 doesn't exist:\nstdout: {}\nstderr: {}",
        stdout,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("1.0.1"),
        "Should produce v1.0.1:\n{}",
        stdout
    );
}

#[test]
fn test_maintenance_collision_still_works() {
    // Regression: maintenance branch collision still detected after universal check
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
        r#"
branches:
  - main
  - name: "1.x"
    maintenance: true
steps: []
"#,
    )
    .unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    git(root, &["tag", "-a", "v1.0.0", "-m", "v1.0.0"]);

    // Release v1.1.0 on main
    fs::write(root.join("index.js"), "// v1.1").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat: feature"]);
    git(root, &["tag", "-a", "v1.1.0", "-m", "v1.1.0"]);

    // Maintenance branch from v1.0.0, feat → would be 1.1.0 → collision
    git(root, &["checkout", "v1.0.0", "-b", "1.x"]);
    fs::write(root.join("index.js"), "// maintenance feat").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat: maintenance feature"]);

    super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists as a tag"));
}
