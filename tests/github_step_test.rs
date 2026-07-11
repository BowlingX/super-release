mod common;

use common::{git, super_release_bin};
use predicates::prelude::*;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

fn setup(root: &Path, release_yaml: &str) {
    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.email", "test@test.com"]);
    git(root, &["config", "user.name", "Test"]);

    fs::write(
        root.join("package.json"),
        r#"{"name": "my-app", "version": "1.0.0"}"#,
    )
    .unwrap();
    fs::write(root.join("index.js"), "// v1").unwrap();
    fs::write(root.join(".release.yaml"), release_yaml).unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    git(root, &["tag", "-a", "v1.0.0", "-m", "v1.0.0"]);

    fs::write(root.join("index.js"), "// v1.1").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat: add feature"]);
}

/// Seed a bare `origin` at the current HEAD so the non-dry-run up-to-date check
/// passes without network.
fn add_remote(dir: &TempDir, root: &Path) {
    let remote = dir.path().join("remote.git");
    git(
        dir.path(),
        &["init", "--bare", "-b", "main", remote.to_str().unwrap()],
    );
    git(root, &["remote", "add", "origin", remote.to_str().unwrap()]);
    git(root, &["push", "origin", "main", "v1.0.0"]);
}

#[test]
fn dry_run_reports_would_create_release() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    setup(
        root,
        "branches: [main]\nsteps:\n  - name: changelog\n  - name: github\n",
    );

    super_release_bin()
        .current_dir(root)
        .arg("--dry-run")
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_TOKEN")
        .assert()
        .success()
        .stdout(predicate::str::contains("[github] Would create release"))
        .stdout(predicate::str::contains("v1.1.0"));
}

#[test]
fn skips_when_push_disabled() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    setup(
        root,
        "branches: [main]\nsteps:\n  - name: changelog\n  - name: github\ngit:\n  push: false\n",
    );
    add_remote(&dir, root);

    // push: false → the step skips before touching the network or needing a token.
    super_release_bin()
        .current_dir(root)
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_TOKEN")
        .assert()
        .success()
        .stdout(predicate::str::contains("git.push is disabled"));
}

#[test]
fn fails_verify_without_token_when_push_enabled() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    setup(
        root,
        "branches: [main]\nsteps:\n  - name: github\ngit:\n  push: true\n",
    );
    add_remote(&dir, root);

    // push: true but no token → fail fast in verify, before any push.
    super_release_bin()
        .current_dir(root)
        .env_remove("GITHUB_TOKEN")
        .env_remove("GH_TOKEN")
        .assert()
        .failure()
        .stderr(predicate::str::contains("GITHUB_TOKEN"));
}
