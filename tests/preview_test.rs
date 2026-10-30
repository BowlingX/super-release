mod common;

use common::{git, super_release_bin};
use predicates::prelude::*;
use std::fs;
use std::process;
use tempfile::TempDir;

/// A monorepo with a root package and one sub-package, both tagged at 1.0.0.
fn setup_repo() -> TempDir {
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
        r#"{"name": "@myorg/core", "version": "1.0.0"}"#,
    )
    .unwrap();
    fs::write(
        root.join("packages/core/src/index.ts"),
        "export const x = 1;",
    )
    .unwrap();
    fs::write(
        root.join(".release.yaml"),
        "branches: [main]\nsteps:\n  - name: changelog\n",
    )
    .unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: initial commit"]);
    git(root, &["tag", "-a", "v1.0.0", "-m", "Release root v1.0.0"]);
    git(
        root,
        &[
            "tag",
            "-a",
            "@myorg/core/v1.0.0",
            "-m",
            "Release @myorg/core v1.0.0",
        ],
    );

    dir
}

fn is_clean(root: &std::path::Path) -> bool {
    let out = process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(root)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().is_empty()
}

#[test]
fn preview_renders_marker_table_and_notes() {
    let dir = setup_repo();
    let root = dir.path();

    // A feature on core → minor bump.
    fs::write(
        root.join("packages/core/src/index.ts"),
        "export const x = 1;\nexport const z = 3;",
    )
    .unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat(core): add export z"]);

    super_release_bin()
        .current_dir(root)
        .args(["--preview", "--base", "main", "--no-comment"])
        .assert()
        .success()
        .stdout(predicate::str::contains("<!-- super-release:preview -->"))
        .stdout(predicate::str::contains("Release preview"))
        .stdout(predicate::str::contains("@myorg/core"))
        .stdout(predicate::str::contains("1.1.0"))
        .stdout(predicate::str::contains("<details>"));

    // Preview must not modify the working tree.
    assert!(is_clean(root), "preview mutated the repository");
}

#[test]
fn preview_reports_no_release_for_non_bumping_commits() {
    let dir = setup_repo();
    let root = dir.path();

    // Only a chore since the tag → no release.
    fs::write(
        root.join("packages/core/src/index.ts"),
        "export const x = 2;",
    )
    .unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore(core): tidy up"]);

    super_release_bin()
        .current_dir(root)
        .args(["--preview", "--base", "main", "--no-comment"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "No release will be triggered by this pull request",
        ));

    assert!(is_clean(root), "preview mutated the repository");
}

/// A previewed PR controls its config: a changelog template must not be able to
/// read the environment (and post a token in the preview comment).
#[test]
fn preview_refuses_templates_that_read_the_environment() {
    let dir = setup_repo();
    let root = dir.path();

    fs::write(
        root.join(".release.yaml"),
        "branches: [main]\nsteps:\n  - name: changelog\n    options:\n      template: \"LEAK {{ get_env(name=\x27SR_PREVIEW_SECRET\x27) }}\"\n",
    )
    .unwrap();
    fs::write(
        root.join("packages/core/src/index.ts"),
        "export const y = 2;",
    )
    .unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat(core): add y"]);

    super_release_bin()
        .current_dir(root)
        .env("SR_PREVIEW_SECRET", "s3cr3t-value")
        .args(["--preview", "--base", "main", "--no-comment"])
        .assert()
        .success()
        .stdout(predicate::str::contains("s3cr3t-value").not())
        .stdout(predicate::str::contains("<details>"))
        .stderr(predicate::str::contains("get_env"));
}

/// `template_file` must stay inside the repository.
#[test]
fn preview_ignores_template_files_outside_the_repo() {
    let dir = setup_repo();
    let root = dir.path();
    let outside = TempDir::new().unwrap();
    let secret = outside.path().join("secret.tera");
    fs::write(&secret, "s3cr3t-file").unwrap();

    fs::write(
        root.join(".release.yaml"),
        format!(
            "branches: [main]\nsteps:\n  - name: changelog\n    options:\n      template_file: \x27{}\x27\n",
            secret.display()
        ),
    )
    .unwrap();
    fs::write(
        root.join("packages/core/src/index.ts"),
        "export const y = 2;",
    )
    .unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat(core): add y"]);

    super_release_bin()
        .current_dir(root)
        .args(["--preview", "--base", "main", "--no-comment"])
        .assert()
        .success()
        .stdout(predicate::str::contains("s3cr3t-file").not());
}
