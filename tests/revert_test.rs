mod common;

use common::{git, super_release_bin};
use predicates::prelude::*;
use std::fs;
use tempfile::TempDir;

/// Single root package repo with a released v1.0.0 and an unreleased feat
/// commit; `git revert HEAD` then produces git's default revert message.
fn setup_repo_with_feat() -> TempDir {
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
    fs::write(root.join("index.js"), "// v1").unwrap();
    fs::write(
        root.join(".release.yaml"),
        "branches:\n  - main\nsteps:\n  - name: changelog\n",
    )
    .unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: initial"]);
    git(root, &["tag", "-a", "v1.0.0", "-m", "v1.0.0"]);

    fs::write(root.join("index.js"), "// v1\n// new feature").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat: add feature"]);

    dir
}

#[test]
fn test_revert_of_released_feat_is_patch() {
    let dir = setup_repo_with_feat();
    let root = dir.path();

    // The feat is released as v1.1.0; the revert alone is in the next range
    // and must yield a patch with a Revert section in the notes.
    git(root, &["tag", "-a", "v1.1.0", "-m", "v1.1.0"]);
    git(root, &["revert", "--no-edit", "HEAD"]);

    super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("1.1.1"))
        .stdout(predicate::str::contains("Revert"))
        .stdout(predicate::str::contains("add feature"));
}

#[test]
fn test_revert_pair_cancels_release() {
    let dir = setup_repo_with_feat();
    let root = dir.path();

    // feat and its revert are both unreleased: they cancel each other out
    // (semantic-release parity) and nothing is released.
    git(root, &["revert", "--no-edit", "HEAD"]);

    super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .assert()
        .success()
        .stdout(predicate::str::contains("No releases needed"));
}
