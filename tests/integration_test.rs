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
