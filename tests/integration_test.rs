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

    // Tag initial versions (new format: {name}/v{version} for sub-packages)
    git(root, &["tag", "-a", "@myorg/core/v1.0.0", "-m", "Release @myorg/core v1.0.0"]);
    git(root, &["tag", "-a", "@myorg/utils/v1.0.0", "-m", "Release @myorg/utils v1.0.0"]);

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
    // Root package uses v{version} format
    git(root, &["tag", "-a", "v1.0.0", "-m", "Release v1.0.0"]);

    super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("No releases needed"));
}

#[test]
fn test_package_include_filter() {
    let dir = setup_monorepo();

    // Override config to only include core
    fs::write(
        dir.path().join(".release.yaml"),
        r#"
branches:
  - main
packages:
  - "@myorg/core"
plugins:
  - name: git-tag
"#,
    )
    .unwrap();

    let output = super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(dir.path().to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "Failed:\n{}", stdout);
    assert!(stdout.contains("@myorg/core"), "Should include core:\n{}", stdout);
    assert!(!stdout.contains("@myorg/utils"), "Should exclude utils:\n{}", stdout);
    assert!(stdout.contains("1.1.0"), "Should have core bump:\n{}", stdout);
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
    // Root package uses v{version} format
    git(root, &["tag", "-a", "v2.0.0", "-m", "v2.0.0"]);

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

#[test]
fn test_first_release_no_prior_tags() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.email", "test@test.com"]);
    git(root, &["config", "user.name", "Test"]);

    fs::write(
        root.join("package.json"),
        r#"{"name": "brand-new-pkg", "version": "0.0.0"}"#,
    )
    .unwrap();
    fs::write(root.join("index.js"), "console.log('hello')").unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat: initial implementation"]);

    fs::write(root.join("index.js"), "console.log('hello world')").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix: typo in greeting"]);

    super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("brand-new-pkg"))
        .stdout(predicate::str::contains("0.1.0")) // feat on 0.x -> minor bump
        .stdout(predicate::str::contains("initial implementation"))
        .stdout(predicate::str::contains("typo in greeting"));
}

#[test]
fn test_dry_run_shows_new_tag_format() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.email", "test@test.com"]);
    git(root, &["config", "user.name", "Test"]);

    // Root package
    fs::write(
        root.join("package.json"),
        r#"{"name": "my-app", "version": "1.0.0"}"#,
    )
    .unwrap();
    fs::write(root.join("index.js"), "// app").unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    git(root, &["tag", "-a", "v1.0.0", "-m", "v1.0.0"]);

    fs::write(root.join("index.js"), "// updated").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat: new feature"]);

    // Root package should produce tag v1.1.0 (not my-app/v1.1.0)
    super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("v1.1.0"));
}

#[test]
fn test_monorepo_tag_format_in_dry_run() {
    let dir = setup_monorepo();

    super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(dir.path().to_str().unwrap())
        .assert()
        .success()
        // Sub-packages use {name}/v{version} format
        .stdout(predicate::str::contains("@myorg/core/v1.1.0"))
        .stdout(predicate::str::contains("@myorg/utils/v1.0.1"));
}

#[test]
fn test_custom_tag_format_package() {
    // Verify that tag_format_package "{name}@{version}" works (e.g. semantic-release compat)
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.email", "test@test.com"]);
    git(root, &["config", "user.name", "Test"]);

    fs::create_dir_all(root.join("packages/lib/src")).unwrap();
    fs::write(
        root.join("packages/lib/package.json"),
        r#"{"name": "my-lib", "version": "1.0.0"}"#,
    )
    .unwrap();
    fs::write(root.join("packages/lib/src/index.ts"), "// v1").unwrap();

    fs::write(
        root.join(".release.yaml"),
        r#"
tag_format_package: "{name}@{version}"
plugins:
  - name: git-tag
"#,
    )
    .unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    git(root, &["tag", "-a", "my-lib@1.0.0", "-m", "v1"]);

    fs::write(root.join("packages/lib/src/index.ts"), "// v1.1").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat: add feature"]);

    super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("1.1.0"))
        .stdout(predicate::str::contains("my-lib@1.1.0"));
}

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
plugins:
  - name: changelog
  - name: git-tag
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
plugins:
  - name: git-tag
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
plugins:
  - name: git-tag
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
plugins:
  - name: git-tag
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
    // Must stay in 1.x range
    assert!(stdout.contains("1.6.0"), "Expected 1.6.0 but got:\n{}", stdout);
    // Must NOT jump to 2.0.0
    assert!(!stdout.contains("2.0.0"), "Should not contain 2.0.0:\n{}", stdout);
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
plugins:
  - name: git-tag
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
plugins:
  - name: git-tag
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
    git(root, &["tag", "-a", "v1.1.0-test-foo.2", "-m", "prerelease"]);

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
plugins:
  - name: git-tag
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

#[test]
fn test_unconfigured_branch_skips_release() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    git(root, &["init", "-b", "develop"]);
    git(root, &["config", "user.email", "test@test.com"]);
    git(root, &["config", "user.name", "Test"]);

    fs::write(
        root.join("package.json"),
        r#"{"name": "my-app", "version": "1.0.0"}"#,
    )
    .unwrap();
    fs::write(root.join("index.js"), "// v1").unwrap();

    // Only test-* branches configured — develop is NOT listed
    fs::write(
        root.join(".release.yaml"),
        r#"
branches:
  - name: "test-*"
    prerelease: true
plugins:
  - name: git-tag
"#,
    )
    .unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    git(root, &["tag", "-a", "v1.0.0", "-m", "v1.0.0"]);

    fs::write(root.join("index.js"), "// v1.1").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat: new feature"]);

    super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("not configured for releases"));
}

#[test]
fn test_merge_with_higher_version_uses_merged_base() {
    // Scenario: test-feature branch merges develop which has v2.0.0.
    // After merge, the base version should be v2.0.0, not v1.0.0.
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
  - name: "test-*"
    prerelease: true
plugins:
  - name: git-tag
"#,
    )
    .unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    git(root, &["tag", "-a", "v1.0.0", "-m", "v1.0.0"]);

    // Advance main to v2.0.0
    fs::write(root.join("index.js"), "// v2").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat!: breaking change"]);
    git(root, &["tag", "-a", "v2.0.0", "-m", "v2.0.0"]);

    // Create test branch from current main (which has v2.0.0)
    git(root, &["checkout", "-b", "test-feature"]);

    // Add a feature on the test branch
    fs::write(root.join("index.js"), "// v2 + test feature").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat: test feature"]);

    // Should use v2.0.0 as base (reachable via merge), not v1.0.0
    let output = super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "Failed:\n{}", stdout);
    // Base should be 2.0.0, next prerelease should be 2.1.0-test-feature.1
    assert!(
        stdout.contains("2.1.0-test-feature.1"),
        "Should use v2.0.0 as base:\n{}",
        stdout
    );
}

#[test]
fn test_unreachable_tag_on_other_branch_ignored() {
    // Scenario: v2.0.0 exists on main but was never merged into test-feature.
    // test-feature should use v1.0.0 as its base, not v2.0.0.
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
  - name: "test-*"
    prerelease: true
plugins:
  - name: git-tag
"#,
    )
    .unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    git(root, &["tag", "-a", "v1.0.0", "-m", "v1.0.0"]);

    // Create test branch BEFORE main advances
    git(root, &["checkout", "-b", "test-feature"]);

    // Go back to main and create v2.0.0 (unreachable from test-feature)
    git(root, &["checkout", "main"]);
    fs::write(root.join("index.js"), "// v2 on main").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat!: breaking on main"]);
    git(root, &["tag", "-a", "v2.0.0", "-m", "v2.0.0"]);

    // Switch back to test-feature and add a commit
    git(root, &["checkout", "test-feature"]);
    fs::write(root.join("index.js"), "// test work").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat: test work"]);

    let output = super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "Failed:\n{}", stdout);
    // Should use v1.0.0 as base (v2.0.0 is not reachable), so next is 1.1.0-test-feature.1
    assert!(
        stdout.contains("1.1.0-test-feature.1"),
        "Should use v1.0.0 as base (v2.0.0 unreachable):\n{}",
        stdout
    );
    assert!(
        !stdout.contains("2."),
        "Should NOT reference v2.x:\n{}",
        stdout
    );
}

#[test]
fn test_behind_remote_blocks_release() {
    // Set up a bare "remote" repo, clone it, then advance the remote.
    // The local clone should be behind and super-release should refuse to release.
    let remote_dir = TempDir::new().unwrap();
    let local_dir = TempDir::new().unwrap();
    let remote = remote_dir.path();
    let local = local_dir.path();

    // Create bare remote
    git(remote, &["init", "--bare", "-b", "main"]);

    // Clone it
    process::Command::new("git")
        .args(["clone", remote.to_str().unwrap(), local.to_str().unwrap()])
        .output()
        .unwrap();

    git(local, &["config", "user.email", "test@test.com"]);
    git(local, &["config", "user.name", "Test"]);

    fs::write(
        local.join("package.json"),
        r#"{"name": "my-app", "version": "1.0.0"}"#,
    )
    .unwrap();
    fs::write(local.join("index.js"), "// v1").unwrap();
    fs::write(
        local.join(".release.yaml"),
        "branches: [main]\nplugins:\n  - name: git-tag\n",
    )
    .unwrap();

    git(local, &["add", "."]);
    git(local, &["commit", "-m", "chore: init"]);
    git(local, &["tag", "-a", "v1.0.0", "-m", "v1.0.0"]);
    git(local, &["push", "origin", "main", "--tags"]);

    // Add a local feature commit
    fs::write(local.join("index.js"), "// v1.1").unwrap();
    git(local, &["add", "."]);
    git(local, &["commit", "-m", "feat: local feature"]);

    // Simulate remote advancing: clone again to a tmp dir, commit, push
    let tmp_clone = TempDir::new().unwrap();
    process::Command::new("git")
        .args([
            "clone",
            remote.to_str().unwrap(),
            tmp_clone.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    git(tmp_clone.path(), &["config", "user.email", "test@test.com"]);
    git(tmp_clone.path(), &["config", "user.name", "Test"]);
    fs::write(tmp_clone.path().join("other.txt"), "remote change").unwrap();
    git(tmp_clone.path(), &["add", "."]);
    git(tmp_clone.path(), &["commit", "-m", "feat: remote commit"]);
    git(tmp_clone.path(), &["push", "origin", "main"]);

    // Fetch so local knows about the remote commit
    git(local, &["fetch", "origin"]);

    // Now local is behind remote — release should fail
    let output = super_release_bin()
        .arg("-C")
        .arg(local.to_str().unwrap())
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        !output.status.success(),
        "Should fail when behind remote:\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );
    assert!(
        stderr.contains("behind") || stdout.contains("behind"),
        "Should mention 'behind':\nstdout: {}\nstderr: {}",
        stdout,
        stderr
    );
}

#[test]
fn test_behind_remote_skipped_in_dry_run() {
    // Same setup as above but with --dry-run — should succeed
    let remote_dir = TempDir::new().unwrap();
    let local_dir = TempDir::new().unwrap();
    let remote = remote_dir.path();
    let local = local_dir.path();

    git(remote, &["init", "--bare", "-b", "main"]);

    process::Command::new("git")
        .args(["clone", remote.to_str().unwrap(), local.to_str().unwrap()])
        .output()
        .unwrap();

    git(local, &["config", "user.email", "test@test.com"]);
    git(local, &["config", "user.name", "Test"]);

    fs::write(
        local.join("package.json"),
        r#"{"name": "my-app", "version": "1.0.0"}"#,
    )
    .unwrap();
    fs::write(local.join("index.js"), "// v1").unwrap();
    fs::write(
        local.join(".release.yaml"),
        "branches: [main]\nplugins:\n  - name: git-tag\n",
    )
    .unwrap();

    git(local, &["add", "."]);
    git(local, &["commit", "-m", "chore: init"]);
    git(local, &["tag", "-a", "v1.0.0", "-m", "v1.0.0"]);
    git(local, &["push", "origin", "main", "--tags"]);

    fs::write(local.join("index.js"), "// v1.1").unwrap();
    git(local, &["add", "."]);
    git(local, &["commit", "-m", "feat: local feature"]);

    // Advance remote
    let tmp_clone = TempDir::new().unwrap();
    process::Command::new("git")
        .args([
            "clone",
            remote.to_str().unwrap(),
            tmp_clone.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    git(tmp_clone.path(), &["config", "user.email", "test@test.com"]);
    git(tmp_clone.path(), &["config", "user.name", "Test"]);
    fs::write(tmp_clone.path().join("other.txt"), "remote change").unwrap();
    git(tmp_clone.path(), &["add", "."]);
    git(tmp_clone.path(), &["commit", "-m", "feat: remote commit"]);
    git(tmp_clone.path(), &["push", "origin", "main"]);

    git(local, &["fetch", "origin"]);

    // Dry-run should still succeed even though we're behind
    super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(local.to_str().unwrap())
        .assert()
        .success();
}
