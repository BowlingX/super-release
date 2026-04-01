mod common;

use common::{git, super_release_bin};
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

#[test]
fn test_maintenance_branch() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    git(root, &["init", "-b", "1.x"]);
    git(root, &["config", "user.email", "test@test.com"]);
    git(root, &["config", "user.name", "Test"]);

    fs::write(
        root.join("package.json"),
        r#"{"name": "my-lib", "version": "1.5.0"}"#,
    )
    .unwrap();
    fs::write(root.join("index.js"), "// v1.5").unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    git(root, &["tag", "-a", "v1.5.0", "-m", "v1.5.0"]);

    // Fix on maintenance branch
    fs::write(root.join("index.js"), "// v1.5 fixed").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix: backport security fix"]);

    fs::write(
        root.join(".release.yaml"),
        r#"
branches:
  - main
  - name: "1.x"
    maintenance: true
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
        .stdout(predicate::str::contains("1.5.1"))
        .stdout(predicate::str::contains("maintenance"));
}

#[test]
fn test_maintenance_branch_caps_breaking_change() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    git(root, &["init", "-b", "1.x"]);
    git(root, &["config", "user.email", "test@test.com"]);
    git(root, &["config", "user.name", "Test"]);

    fs::write(
        root.join("package.json"),
        r#"{"name": "my-lib", "version": "1.5.0"}"#,
    )
    .unwrap();
    fs::write(root.join("index.js"), "// v1.5").unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    git(root, &["tag", "-a", "v1.5.0", "-m", "v1.5.0"]);

    // Breaking change on maintenance branch — should NOT bump to 2.0.0
    fs::write(root.join("index.js"), "// breaking").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat!: breaking but capped"]);

    fs::write(
        root.join(".release.yaml"),
        r#"
branches:
  - name: "1.x"
    maintenance: true
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
    // Branch 1.x — major is locked, breaking change capped to minor bump
    assert!(
        stdout.contains("1.6.0"),
        "Expected 1.6.0 (breaking capped to minor on 1.x) but got:\n{}",
        stdout
    );
    // Must NOT jump to 2.0.0
    assert!(
        !stdout.contains("2.0.0"),
        "Should not contain 2.0.0:\n{}",
        stdout
    );
}

#[test]
fn test_maintenance_major_minor_branch_caps_to_patch() {
    // Branch `1.5.x` — both major and minor are locked, feat should become patch
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    git(root, &["init", "-b", "1.5.x"]);
    git(root, &["config", "user.email", "test@test.com"]);
    git(root, &["config", "user.name", "Test"]);

    fs::write(
        root.join("package.json"),
        r#"{"name": "my-lib", "version": "1.5.0"}"#,
    )
    .unwrap();
    fs::write(root.join("index.js"), "// v1.5").unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    git(root, &["tag", "-a", "v1.5.0", "-m", "v1.5.0"]);

    // feat on a major.minor.x branch — should be capped to patch
    fs::write(root.join("index.js"), "// new feature").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat: add feature"]);

    fs::write(
        root.join(".release.yaml"),
        r#"
branches:
  - name: "*.*.x"
    maintenance: true
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
    // feat capped to patch on 1.5.x
    assert!(
        stdout.contains("1.5.1"),
        "Expected 1.5.1 (feat capped to patch on 1.5.x) but got:\n{}",
        stdout
    );
    assert!(
        !stdout.contains("1.6.0"),
        "Should not bump minor on 1.5.x branch:\n{}",
        stdout
    );
}

#[test]
fn test_maintenance_range_mismatch_skips_package() {
    // Branch `1.5.x` but version is 2.0.0 — should skip (no release), not error
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    git(root, &["init", "-b", "1.5.x"]);
    git(root, &["config", "user.email", "test@test.com"]);
    git(root, &["config", "user.name", "Test"]);

    fs::write(
        root.join("package.json"),
        r#"{"name": "my-lib", "version": "2.0.0"}"#,
    )
    .unwrap();
    fs::write(root.join("index.js"), "// v2").unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    git(root, &["tag", "-a", "v2.0.0", "-m", "v2.0.0"]);

    fs::write(root.join("index.js"), "// fix").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix: something"]);

    fs::write(
        root.join(".release.yaml"),
        r#"
branches:
  - name: "*.*.x"
    maintenance: true
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
    assert!(
        output.status.success(),
        "Should succeed (skip out-of-range package, not error)"
    );
    assert!(
        stdout.contains("No releases needed"),
        "Should report no releases (package skipped):\n{}",
        stdout
    );
}

#[test]
fn test_maintenance_version_collision_detected() {
    // main has v1.0.0 through v1.5.0. Branch 1.x at v1.4.0 does feat: → would be 1.5.0 → ERROR
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.email", "test@test.com"]);
    git(root, &["config", "user.name", "Test"]);

    fs::write(
        root.join("package.json"),
        r#"{"name": "my-lib", "version": "1.0.0"}"#,
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

    // Releases on main up to v1.5.0
    for minor in 1..=5 {
        fs::write(root.join("index.js"), format!("// v1.{}", minor)).unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-m", "feat: feature"]);
        git(
            root,
            &[
                "tag",
                "-a",
                &format!("v1.{}.0", minor),
                "-m",
                &format!("v1.{}.0", minor),
            ],
        );
    }

    // Create maintenance branch from v1.4.0
    git(root, &["checkout", "v1.4.0", "-b", "1.x"]);

    // feat commit → would produce 1.5.0 which already exists
    fs::write(root.join("index.js"), "// maintenance feat").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat: maintenance feature"]);

    let output = super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "Should fail when version collides with existing tag"
    );
    assert!(
        stderr.contains("already exists as a tag"),
        "Should mention collision:\n{}",
        stderr
    );
}

#[test]
fn test_maintenance_no_collision_when_gap_exists() {
    // main has v1.0.0 and v2.0.0. Branch 1.x at v1.0.0 does feat: → 1.1.0 → OK
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.email", "test@test.com"]);
    git(root, &["config", "user.name", "Test"]);

    fs::write(
        root.join("package.json"),
        r#"{"name": "my-lib", "version": "1.0.0"}"#,
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

    // Jump straight to v2.0.0 on main
    fs::write(root.join("index.js"), "// v2").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat!: breaking change"]);
    git(root, &["tag", "-a", "v2.0.0", "-m", "v2.0.0"]);

    // Create maintenance branch from v1.0.0
    git(root, &["checkout", "v1.0.0", "-b", "1.x"]);

    fs::write(root.join("index.js"), "// maintenance feat").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat: maintenance feature"]);

    let output = super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "Should succeed — 1.1.0 doesn't exist:\nstdout: {}\nstderr: {}",
        stdout,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("1.1.0"),
        "Should produce 1.1.0:\n{}",
        stdout
    );
}

#[test]
fn test_maintenance_patch_collision_detected() {
    // main has v1.0.0 and v1.0.1. Branch 1.0.x at v1.0.0 does fix: → 1.0.1 → ERROR
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.email", "test@test.com"]);
    git(root, &["config", "user.name", "Test"]);

    fs::write(
        root.join("package.json"),
        r#"{"name": "my-lib", "version": "1.0.0"}"#,
    )
    .unwrap();
    fs::write(root.join("index.js"), "// v1").unwrap();
    fs::write(
        root.join(".release.yaml"),
        r#"
branches:
  - main
  - name: "*.*.x"
    maintenance: true
steps: []
"#,
    )
    .unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    git(root, &["tag", "-a", "v1.0.0", "-m", "v1.0.0"]);

    // Patch release on main
    fs::write(root.join("index.js"), "// fix on main").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix: bug on main"]);
    git(root, &["tag", "-a", "v1.0.1", "-m", "v1.0.1"]);

    // Create maintenance branch from v1.0.0
    git(root, &["checkout", "v1.0.0", "-b", "1.0.x"]);

    fs::write(root.join("index.js"), "// fix on maintenance").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix: bug on maintenance"]);

    let output = super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "Should fail — v1.0.1 already exists"
    );
    assert!(
        stderr.contains("already exists as a tag"),
        "Should mention collision:\n{}",
        stderr
    );
}

#[test]
fn test_maintenance_no_collision_after_major_bump() {
    // main has v1.0.0→v1.5.0→v2.0.0. Branch 1.x at v1.5.0 does feat: → 1.6.0 → OK
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.email", "test@test.com"]);
    git(root, &["config", "user.name", "Test"]);

    fs::write(
        root.join("package.json"),
        r#"{"name": "my-lib", "version": "1.0.0"}"#,
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

    // Releases on main: 1.5.0, then 2.0.0
    fs::write(root.join("index.js"), "// v1.5").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat: feature"]);
    git(root, &["tag", "-a", "v1.5.0", "-m", "v1.5.0"]);

    fs::write(root.join("index.js"), "// v2").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat!: breaking"]);
    git(root, &["tag", "-a", "v2.0.0", "-m", "v2.0.0"]);

    // Maintenance branch from v1.5.0
    git(root, &["checkout", "v1.5.0", "-b", "1.x"]);

    fs::write(root.join("index.js"), "// maintenance feat").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat: maintenance feature"]);

    let output = super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "Should succeed — 1.6.0 doesn't exist:\nstdout: {}\nstderr: {}",
        stdout,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("1.6.0"),
        "Should produce 1.6.0:\n{}",
        stdout
    );
}

#[test]
fn test_maintenance_monorepo_skips_out_of_range_packages() {
    // Monorepo: @acme/core at v3.0.0, @acme/utils at v1.2.0.
    // Branch 1.x should skip core (out of range) and only release utils.
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.email", "test@test.com"]);
    git(root, &["config", "user.name", "Test"]);

    // Root package
    fs::write(
        root.join("package.json"),
        r#"{"name": "monorepo-root", "version": "1.0.0", "private": true}"#,
    )
    .unwrap();

    // @acme/core at v3.0.0
    fs::create_dir_all(root.join("packages/core/src")).unwrap();
    fs::write(
        root.join("packages/core/package.json"),
        r#"{"name": "@acme/core", "version": "3.0.0"}"#,
    )
    .unwrap();
    fs::write(root.join("packages/core/src/index.ts"), "// v3").unwrap();

    // @acme/utils at v1.2.0
    fs::create_dir_all(root.join("packages/utils/src")).unwrap();
    fs::write(
        root.join("packages/utils/package.json"),
        r#"{"name": "@acme/utils", "version": "1.2.0"}"#,
    )
    .unwrap();
    fs::write(root.join("packages/utils/src/index.ts"), "// v1.2").unwrap();

    fs::write(
        root.join(".release.yaml"),
        r#"
branches:
  - main
  - name: "1.x"
    maintenance: true
exclude:
  - monorepo-root
steps: []
"#,
    )
    .unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    git(
        root,
        &["tag", "-a", "@acme/core/v3.0.0", "-m", "core v3.0.0"],
    );
    git(
        root,
        &["tag", "-a", "@acme/utils/v1.2.0", "-m", "utils v1.2.0"],
    );

    // Create maintenance branch
    git(root, &["checkout", "-b", "1.x"]);

    // Fix in both packages
    fs::write(root.join("packages/core/src/index.ts"), "// v3 fix").unwrap();
    fs::write(root.join("packages/utils/src/index.ts"), "// v1.2 fix").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix: bugfix in both packages"]);

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
    // utils should get a release (v1.2.1)
    assert!(
        stdout.contains("@acme/utils"),
        "Should release utils:\n{}",
        stdout
    );
    assert!(
        stdout.contains("1.2.1"),
        "Utils should bump to 1.2.1:\n{}",
        stdout
    );
    // core should be skipped (v3.0.0 is outside 1.x range)
    assert!(
        !stdout.contains("@acme/core") || !stdout.contains("3.0.1"),
        "Should not release core (out of 1.x range):\n{}",
        stdout
    );
}

#[test]
fn test_maintenance_monorepo_with_packages_filter() {
    // Same as above but with explicit packages filter on the maintenance branch.
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.email", "test@test.com"]);
    git(root, &["config", "user.name", "Test"]);

    fs::write(
        root.join("package.json"),
        r#"{"name": "monorepo-root", "version": "1.0.0", "private": true}"#,
    )
    .unwrap();

    fs::create_dir_all(root.join("packages/core/src")).unwrap();
    fs::write(
        root.join("packages/core/package.json"),
        r#"{"name": "@acme/core", "version": "3.0.0"}"#,
    )
    .unwrap();
    fs::write(root.join("packages/core/src/index.ts"), "// v3").unwrap();

    fs::create_dir_all(root.join("packages/utils/src")).unwrap();
    fs::write(
        root.join("packages/utils/package.json"),
        r#"{"name": "@acme/utils", "version": "1.2.0"}"#,
    )
    .unwrap();
    fs::write(root.join("packages/utils/src/index.ts"), "// v1.2").unwrap();

    fs::write(
        root.join(".release.yaml"),
        r#"
branches:
  - main
  - name: "1.x"
    maintenance: true
    packages:
      - "@acme/utils"
exclude:
  - monorepo-root
steps: []
"#,
    )
    .unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    git(
        root,
        &["tag", "-a", "@acme/core/v3.0.0", "-m", "core v3.0.0"],
    );
    git(
        root,
        &["tag", "-a", "@acme/utils/v1.2.0", "-m", "utils v1.2.0"],
    );

    git(root, &["checkout", "-b", "1.x"]);

    fs::write(root.join("packages/core/src/index.ts"), "// v3 fix").unwrap();
    fs::write(root.join("packages/utils/src/index.ts"), "// v1.2 fix").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix: bugfix in both"]);

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
        stdout.contains("@acme/utils"),
        "Should release utils:\n{}",
        stdout
    );
    assert!(
        stdout.contains("1.2.1"),
        "Utils should bump to 1.2.1:\n{}",
        stdout
    );
    // core is excluded by branch packages filter
    assert!(
        !stdout.contains("@acme/core") || !stdout.contains("3.0.1"),
        "Should not release core (filtered by packages config):\n{}",
        stdout
    );
}
