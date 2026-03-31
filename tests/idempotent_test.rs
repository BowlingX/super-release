use assert_cmd::Command;
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

fn setup_single_package(root: &Path) {
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
plugins:
  - name: changelog
"#,
    )
    .unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    git(root, &["tag", "-a", "v1.0.0", "-m", "v1.0.0"]);

    fs::write(root.join("index.js"), "// v1.1").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat: add new feature"]);
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

    fs::create_dir_all(root.join("packages/utils/src")).unwrap();
    fs::write(
        root.join("packages/utils/package.json"),
        r#"{"name": "@test/utils", "version": "1.0.0", "dependencies": {"@test/core": "^1.0.0"}}"#,
    )
    .unwrap();
    fs::write(root.join("packages/utils/src/index.ts"), "// v1").unwrap();

    fs::write(
        root.join(".release.yaml"),
        r#"
branches:
  - main
exclude:
  - mono-root
plugins:
  - name: changelog
"#,
    )
    .unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    git(root, &["tag", "-a", "@test/core/v1.0.0", "-m", "v1"]);
    git(root, &["tag", "-a", "@test/utils/v1.0.0", "-m", "v1"]);

    fs::write(root.join("packages/core/src/index.ts"), "// v1.1").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat(core): add feature"]);

    fs::write(root.join("packages/utils/src/index.ts"), "// v1 fix").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix(utils): fix bug"]);
}

// ──────────────────────────────────────────────────────────────
// Full release run (non-dry-run) — single package
// ──────────────────────────────────────────────────────────────

#[test]
fn test_full_release_single_package() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    setup_single_package(root);

    let output = super_release_bin()
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "Release failed:\nstdout: {}\nstderr: {}", stdout, stderr);

    // Changelog was written
    let changelog = fs::read_to_string(root.join("CHANGELOG.md")).unwrap();
    assert!(changelog.contains("1.1.0"), "Changelog missing version:\n{}", changelog);
    assert!(changelog.contains("new feature"), "Changelog missing commit:\n{}", changelog);

    // Git commit was created
    let log = process::Command::new("git")
        .args(["log", "--oneline", "-1"])
        .current_dir(root)
        .output()
        .unwrap();
    let log_msg = String::from_utf8_lossy(&log.stdout);
    assert!(
        log_msg.contains("chore(release)"),
        "Missing release commit:\n{}",
        log_msg
    );

    // Tag was created
    let tags = process::Command::new("git")
        .args(["tag", "-l", "v1.1.0"])
        .current_dir(root)
        .output()
        .unwrap();
    let tag_list = String::from_utf8_lossy(&tags.stdout);
    assert!(tag_list.contains("v1.1.0"), "Missing tag:\n{}", tag_list);
}

// ──────────────────────────────────────────────────────────────
// Idempotent rerun — running twice produces the same result
// ──────────────────────────────────────────────────────────────

#[test]
fn test_only_plugin_files_are_committed() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    setup_single_package(root);

    // Create an untracked file that should NOT be committed
    fs::write(root.join("untracked.txt"), "should not be committed").unwrap();

    let output = super_release_bin()
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "Failed:\n{}", stdout);

    // The release commit should exist
    let log = process::Command::new("git")
        .args(["log", "--format=%s", "-1"])
        .current_dir(root)
        .output()
        .unwrap();
    let msg = String::from_utf8_lossy(&log.stdout);
    assert!(msg.contains("chore(release)"), "Should have release commit:\n{}", msg);

    // The CHANGELOG.md should be in the commit
    let show = process::Command::new("git")
        .args(["diff-tree", "--no-commit-id", "--name-only", "-r", "HEAD"])
        .current_dir(root)
        .output()
        .unwrap();
    let files = String::from_utf8_lossy(&show.stdout);
    assert!(
        files.contains("CHANGELOG.md"),
        "CHANGELOG.md should be in the commit:\n{}",
        files
    );

    // The untracked file should NOT be in the commit
    assert!(
        !files.contains("untracked.txt"),
        "untracked.txt should NOT be in the commit:\n{}",
        files
    );

    // The untracked file should still exist on disk
    assert!(root.join("untracked.txt").exists());
}

#[test]
fn test_idempotent_rerun_single_package() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    setup_single_package(root);

    // First run
    let output = super_release_bin()
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "First run failed:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );

    // Second run — should succeed with "no releases needed"
    // because the tag v1.1.0 now exists and there are no new commits
    let output = super_release_bin()
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "Second run failed:\n{}", stdout);
    assert!(
        stdout.contains("No releases needed"),
        "Second run should find nothing to release:\n{}",
        stdout
    );
}

// ──────────────────────────────────────────────────────────────
// Idempotent rerun — monorepo
// ──────────────────────────────────────────────────────────────

#[test]
fn test_idempotent_rerun_monorepo() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    setup_monorepo(root);

    // First run
    let output = super_release_bin()
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "First run failed:\n{}", stdout);
    assert!(stdout.contains("@test/core"), "Missing core:\n{}", stdout);
    assert!(stdout.contains("@test/utils"), "Missing utils:\n{}", stdout);

    // Verify artifacts
    assert!(root.join("packages/core/CHANGELOG.md").exists());
    assert!(root.join("packages/utils/CHANGELOG.md").exists());

    let tags = process::Command::new("git")
        .args(["tag", "-l"])
        .current_dir(root)
        .output()
        .unwrap();
    let tag_list = String::from_utf8_lossy(&tags.stdout);
    assert!(tag_list.contains("@test/core/v1.1.0"), "Missing core tag:\n{}", tag_list);
    assert!(tag_list.contains("@test/utils/v1.0.1"), "Missing utils tag:\n{}", tag_list);

    // Second run
    let output = super_release_bin()
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "Second run failed:\n{}", stdout);
    assert!(
        stdout.contains("No releases needed"),
        "Second run should find nothing to release:\n{}",
        stdout
    );
}

// ──────────────────────────────────────────────────────────────
// Git tag idempotency — tags already exist
// ──────────────────────────────────────────────────────────────

#[test]
fn test_git_tag_skips_existing() {
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
plugins: []
"#,
    )
    .unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    git(root, &["tag", "-a", "v1.0.0", "-m", "v1.0.0"]);

    fs::write(root.join("index.js"), "// v1.1").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat: new feature"]);

    // First run — creates tag
    let output = super_release_bin()
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "First run failed:\n{}", stdout);
    assert!(stdout.contains("Created tag: v1.1.0"), "Should create tag:\n{}", stdout);

    // Manually add another commit so the tool finds something to do
    fs::write(root.join("index.js"), "// v1.2").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat: another feature"]);

    // Second run — tag v1.1.0 already exists, should skip it and create v1.2.0
    let output = super_release_bin()
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "Second run failed:\n{}", stdout);
    // v1.2.0 should be the new release (from v1.1.0 base)
    assert!(
        stdout.contains("Created tag"),
        "Should create new tag:\n{}",
        stdout
    );
}

// ──────────────────────────────────────────────────────────────
// Git commit idempotency — nothing to commit
// ──────────────────────────────────────────────────────────────

#[test]
fn test_git_commit_handles_nothing_to_commit() {
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

    // No plugins — core git will still try to commit/tag but no files changed
    fs::write(
        root.join(".release.yaml"),
        r#"
branches:
  - main
plugins: []
"#,
    )
    .unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    git(root, &["tag", "-a", "v1.0.0", "-m", "v1.0.0"]);

    fs::write(root.join("index.js"), "// v1.1").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat: new feature"]);

    // Core git will find nothing to commit since no plugins modified files
    let output = super_release_bin()
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "Should succeed even with nothing to commit:\n{}",
        stdout
    );
}

// ──────────────────────────────────────────────────────────────
// Commit message format
// ──────────────────────────────────────────────────────────────

#[test]
fn test_release_commit_message_format() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    setup_single_package(root);

    super_release_bin()
        .arg("-C")
        .arg(root.to_str().unwrap())
        .assert()
        .success();

    let log = process::Command::new("git")
        .args(["log", "--oneline", "-1"])
        .current_dir(root)
        .output()
        .unwrap();
    let msg = String::from_utf8_lossy(&log.stdout);

    assert!(msg.contains("chore(release)"), "Should have release prefix:\n{}", msg);
    assert!(msg.contains("my-app@1.1.0"), "Should mention package version:\n{}", msg);
    assert!(msg.contains("[skip ci]"), "Should have skip ci:\n{}", msg);
}

#[test]
fn test_release_commit_message_monorepo() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    setup_monorepo(root);

    super_release_bin()
        .arg("-C")
        .arg(root.to_str().unwrap())
        .assert()
        .success();

    let log = process::Command::new("git")
        .args(["log", "--format=%s", "-1"])
        .current_dir(root)
        .output()
        .unwrap();
    let msg = String::from_utf8_lossy(&log.stdout);

    assert!(msg.contains("@test/core@1.1.0"), "Should mention core:\n{}", msg);
    assert!(msg.contains("@test/utils@1.0.1"), "Should mention utils:\n{}", msg);
}

// ──────────────────────────────────────────────────────────────
// Dry run doesn't modify anything
// ──────────────────────────────────────────────────────────────

#[test]
fn test_dry_run_is_readonly() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    setup_single_package(root);

    // Capture state before
    let pkg_before = fs::read_to_string(root.join("package.json")).unwrap();
    let tags_before = process::Command::new("git")
        .args(["tag", "-l"])
        .current_dir(root)
        .output()
        .unwrap();
    let log_before = process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()
        .unwrap();

    // Dry run
    super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .assert()
        .success();

    // Verify nothing changed
    let pkg_after = fs::read_to_string(root.join("package.json")).unwrap();
    assert_eq!(pkg_before, pkg_after, "package.json should not change in dry-run");

    assert!(!root.join("CHANGELOG.md").exists(), "CHANGELOG should not be created in dry-run");

    let tags_after = process::Command::new("git")
        .args(["tag", "-l"])
        .current_dir(root)
        .output()
        .unwrap();
    assert_eq!(
        tags_before.stdout, tags_after.stdout,
        "Tags should not change in dry-run"
    );

    let log_after = process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()
        .unwrap();
    assert_eq!(
        log_before.stdout, log_after.stdout,
        "HEAD should not change in dry-run"
    );
}
