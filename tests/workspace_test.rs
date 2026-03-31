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

fn init_git(root: &Path) {
    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.email", "test@test.com"]);
    git(root, &["config", "user.name", "Test"]);
}

/// Create a monorepo with interdependencies:
///   @test/core (no deps)
///   @test/utils (depends on @test/core)
///   @test/app (depends on @test/utils)
fn create_monorepo_packages(root: &Path, dep_prefix: &str) {
    // packages/core
    fs::create_dir_all(root.join("packages/core/src")).unwrap();
    fs::write(
        root.join("packages/core/package.json"),
        r#"{"name": "@test/core", "version": "1.0.0"}"#,
    )
    .unwrap();
    fs::write(root.join("packages/core/src/index.ts"), "export const v = 1;").unwrap();

    // packages/utils depends on core
    fs::create_dir_all(root.join("packages/utils/src")).unwrap();
    fs::write(
        root.join("packages/utils/package.json"),
        format!(
            r#"{{"name": "@test/utils", "version": "1.0.0", "dependencies": {{"@test/core": "{}1.0.0"}}}}"#,
            dep_prefix
        ),
    )
    .unwrap();
    fs::write(root.join("packages/utils/src/index.ts"), "export const u = 1;").unwrap();

    // packages/app depends on utils
    fs::create_dir_all(root.join("packages/app/src")).unwrap();
    fs::write(
        root.join("packages/app/package.json"),
        format!(
            r#"{{"name": "@test/app", "version": "1.0.0", "dependencies": {{"@test/utils": "{}1.0.0"}}}}"#,
            dep_prefix
        ),
    )
    .unwrap();
    fs::write(root.join("packages/app/src/index.ts"), "export const a = 1;").unwrap();
}

fn write_release_config(root: &Path, extra: &str) {
    fs::write(
        root.join(".release.yaml"),
        format!(
            r#"
branches:
  - main
exclude:
  - test-monorepo
plugins:
  - name: changelog
  - name: npm
    {}
"#,
            extra
        ),
    )
    .unwrap();
}

fn tag_initial_versions(root: &Path) {
    git(root, &["tag", "-a", "@test/core/v1.0.0", "-m", "v1"]);
    git(root, &["tag", "-a", "@test/utils/v1.0.0", "-m", "v1"]);
    git(root, &["tag", "-a", "@test/app/v1.0.0", "-m", "v1"]);
}

fn add_commits(root: &Path) {
    // Feature in core -> should bump core, and utils/app should update deps
    fs::write(
        root.join("packages/core/src/index.ts"),
        "export const v = 1;\nexport function hello() {}",
    )
    .unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat(core): add hello function"]);

    // Fix in utils
    fs::write(
        root.join("packages/utils/src/index.ts"),
        "export const u = 1;\n// fixed",
    )
    .unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix(utils): fix edge case"]);
}

// ──────────────────────────────────────────────────────────────
// npm workspace tests
// ──────────────────────────────────────────────────────────────

#[test]
fn test_npm_workspace_dry_run() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    init_git(root);

    // npm workspaces: root package.json with workspaces field
    fs::write(
        root.join("package.json"),
        r#"{"name": "test-monorepo", "private": true, "workspaces": ["packages/*"]}"#,
    )
    .unwrap();

    create_monorepo_packages(root, "^");
    write_release_config(root, "");

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    tag_initial_versions(root);
    add_commits(root);

    // Should auto-detect npm (no lock file = fallback to npm)
    let output = super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success(), "Failed:\n{}", stdout);
    assert!(stdout.contains("@test/core"), "Missing core:\n{}", stdout);
    assert!(stdout.contains("1.1.0"), "Missing core bump:\n{}", stdout);
    assert!(stdout.contains("@test/utils"), "Missing utils:\n{}", stdout);
    assert!(
        stdout.contains("[npm]"),
        "Should detect npm:\n{}",
        stdout
    );
}

#[test]
fn test_npm_workspace_with_package_lock() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    init_git(root);

    fs::write(
        root.join("package.json"),
        r#"{"name": "test-monorepo", "private": true, "workspaces": ["packages/*"]}"#,
    )
    .unwrap();
    // Create package-lock.json to trigger npm detection
    fs::write(root.join("package-lock.json"), "{}").unwrap();

    create_monorepo_packages(root, "^");
    write_release_config(root, "");

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    tag_initial_versions(root);
    add_commits(root);

    super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("[npm]"));
}

// ──────────────────────────────────────────────────────────────
// yarn workspace tests
// ──────────────────────────────────────────────────────────────

#[test]
fn test_yarn_workspace_detection() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    init_git(root);

    fs::write(
        root.join("package.json"),
        r#"{"name": "test-monorepo", "private": true, "workspaces": ["packages/*"]}"#,
    )
    .unwrap();
    fs::write(root.join("yarn.lock"), "").unwrap();

    create_monorepo_packages(root, "^");
    write_release_config(root, "");

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    tag_initial_versions(root);
    add_commits(root);

    super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("[yarn]"));
}

#[test]
fn test_dependencies_never_rewritten_in_dry_run() {
    // Dependencies are managed by the package manager, super-release only updates version
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    init_git(root);

    fs::write(
        root.join("package.json"),
        r#"{"name": "test-monorepo", "private": true, "workspaces": ["packages/*"]}"#,
    )
    .unwrap();

    create_monorepo_packages(root, "workspace:");
    write_release_config(root, "");

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    tag_initial_versions(root);
    add_commits(root);

    let output = super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "Failed:\n{}", stdout);
    // Only version updates, no dependency rewrites
    assert!(
        !stdout.contains("Would update dependency"),
        "Should not mention dependency updates:\n{}",
        stdout
    );
    assert!(
        stdout.contains("Would update"),
        "Should show version updates:\n{}",
        stdout
    );
}

// ──────────────────────────────────────────────────────────────
// pnpm workspace tests
// ──────────────────────────────────────────────────────────────

#[test]
fn test_pnpm_workspace_detection() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    init_git(root);

    fs::write(
        root.join("package.json"),
        r#"{"name": "test-monorepo", "private": true}"#,
    )
    .unwrap();
    fs::write(root.join("pnpm-lock.yaml"), "").unwrap();
    fs::write(
        root.join("pnpm-workspace.yaml"),
        "packages:\n  - 'packages/*'",
    )
    .unwrap();

    create_monorepo_packages(root, "workspace:");
    write_release_config(root, "");

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    tag_initial_versions(root);
    add_commits(root);

    super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("[pnpm]"));
}

// ──────────────────────────────────────────────────────────────
// packageManager field detection
// ──────────────────────────────────────────────────────────────

#[test]
fn test_package_manager_field_detection() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    init_git(root);

    // packageManager field should take priority over lock files
    fs::write(
        root.join("package.json"),
        r#"{"name": "test-monorepo", "private": true, "packageManager": "pnpm@9.0.0"}"#,
    )
    .unwrap();
    // Put a yarn.lock to verify packageManager wins
    fs::write(root.join("yarn.lock"), "").unwrap();

    create_monorepo_packages(root, "^");
    write_release_config(root, "");

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    tag_initial_versions(root);
    add_commits(root);

    super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("[pnpm]"));
}

// ──────────────────────────────────────────────────────────────
// Manual package_manager override
// ──────────────────────────────────────────────────────────────

#[test]
fn test_package_manager_override_in_config() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    init_git(root);

    fs::write(
        root.join("package.json"),
        r#"{"name": "test-monorepo", "private": true}"#,
    )
    .unwrap();
    // No lock file, would default to npm
    create_monorepo_packages(root, "^");
    // Override to yarn in config
    write_release_config(root, "options:\n      package_manager: yarn");

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    tag_initial_versions(root);
    add_commits(root);

    super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("[yarn]"));
}

// ──────────────────────────────────────────────────────────────
// Dependency ordering
// ──────────────────────────────────────────────────────────────

#[test]
fn test_publish_order_respects_dependencies() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    init_git(root);

    fs::write(
        root.join("package.json"),
        r#"{"name": "test-monorepo", "private": true}"#,
    )
    .unwrap();

    create_monorepo_packages(root, "^");
    write_release_config(root, "");

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    tag_initial_versions(root);

    // Change all three packages
    fs::write(root.join("packages/core/src/index.ts"), "// v2").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat(core): breaking"]);

    fs::write(root.join("packages/utils/src/index.ts"), "// v2").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat(utils): new api"]);

    fs::write(root.join("packages/app/src/index.ts"), "// v2").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat(app): new feature"]);

    let output = super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "Failed:\n{}", stdout);

    // core should be published before utils, utils before app
    let publish_lines: Vec<&str> = stdout
        .lines()
        .filter(|l| l.contains("Would publish"))
        .collect();

    assert!(
        publish_lines.len() >= 3,
        "Expected 3 publish lines, got: {:?}",
        publish_lines
    );

    let core_pos = publish_lines.iter().position(|l| l.contains("@test/core"));
    let utils_pos = publish_lines.iter().position(|l| l.contains("@test/utils"));
    let app_pos = publish_lines.iter().position(|l| l.contains("@test/app"));

    assert!(
        core_pos < utils_pos && utils_pos < app_pos,
        "Wrong publish order: core={:?} utils={:?} app={:?}\nLines: {:?}",
        core_pos,
        utils_pos,
        app_pos,
        publish_lines
    );
}
