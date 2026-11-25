mod common;

use common::{git, super_release_bin};
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

const CONFIG: &str = r#"
branches:
  - main
exclude:
  - mono-root
steps: []
"#;

/// Two packages `@test/a` and `@test/b`, both tagged 1.0.0 on the first commit.
fn setup(root: &Path) {
    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.email", "test@test.com"]);
    git(root, &["config", "user.name", "Test"]);

    fs::write(
        root.join("package.json"),
        r#"{"name": "mono-root", "version": "0.0.0", "private": true}"#,
    )
    .unwrap();
    for name in ["a", "b"] {
        fs::create_dir_all(root.join(format!("packages/{name}"))).unwrap();
        set_version(root, name, "1.0.0");
    }
    fs::write(root.join(".release.yaml"), CONFIG).unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    tag(root, "@test/a/v1.0.0");
    tag(root, "@test/b/v1.0.0");
}

fn set_version(root: &Path, name: &str, version: &str) {
    fs::write(
        root.join(format!("packages/{name}/package.json")),
        format!(r#"{{"name": "@test/{name}", "version": "{version}"}}"#),
    )
    .unwrap();
}

fn tag(root: &Path, name: &str) {
    git(root, &["tag", "-a", name, &format!("-m{name}")]);
}

/// Commit a change to `packages/<name>/<file>`.
fn change(root: &Path, name: &str, file: &str, message: &str) {
    fs::write(root.join(format!("packages/{name}/{file}")), message).unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", message]);
}

/// Commit a `feat` to the package, then tag its 1.1.0 on a commit with
/// `message`, dated `date` when given.
fn release(root: &Path, name: &str, message: &str, date: Option<&str>) {
    change(
        root,
        name,
        "feature.txt",
        &format!("feat: change in {name}"),
    );
    set_version(root, name, "1.1.0");
    git(root, &["add", "."]);
    let mut commit = Command::new("git");
    commit
        .args(["commit", "-q", "-m", message])
        .current_dir(root);
    if let Some(date) = date {
        commit.env("GIT_COMMITTER_DATE", date);
    }
    assert!(commit.status().unwrap().success());
    tag(root, &format!("@test/{name}/v1.1.0"));
}

fn merge(root: &Path, branch: &str) {
    let message = format!("Merge branch '{branch}'");
    git(root, &["merge", "-q", "--no-ff", "-m", &message, branch]);
}

/// Release `b` 1.1.0 on `main` while `develop` carries an unreleased fix to it.
fn release_b_beside_develop(root: &Path) {
    git(root, &["checkout", "-q", "-b", "develop"]);
    change(root, "b", "develop.txt", "fix: develop-only change in b");

    git(root, &["checkout", "-q", "main"]);
    release(root, "b", "chore(release): @test/b@1.1.0", None);
}

fn next_version(root: &Path, package: &str) -> String {
    let output = super_release_bin()
        .args(["--show-next-version", "--package", package, "-C"])
        .arg(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// `main` is merged back into `develop` and fast-forwarded: the tagged commit
/// is listed right behind the merge, ahead of the unreleased develop commit.
#[test]
fn test_commits_merged_around_a_release_tag_are_released() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    setup(root);
    release_b_beside_develop(root);

    git(root, &["checkout", "-q", "develop"]);
    merge(root, "main");
    git(root, &["checkout", "-q", "main"]);
    git(root, &["merge", "-q", "--ff-only", "develop"]);

    // Only the develop-only fix is unreleased; the feat is part of 1.1.0.
    assert_eq!(next_version(root, "@test/b"), "1.1.1");
    assert_eq!(next_version(root, "@test/a"), "1.0.0");
}

/// The unreleased change is on the second parent of the merge.
#[test]
fn test_commits_merged_into_the_release_branch_are_released() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    setup(root);
    release_b_beside_develop(root);

    merge(root, "develop");

    assert_eq!(next_version(root, "@test/b"), "1.1.1");
    assert_eq!(next_version(root, "@test/a"), "1.0.0");
}

/// `a` and `b` are released on two branches that are then merged, and `b`'s
/// tag has the older commit date. Neither tag is behind the other, so the
/// fix to `a` beside `b`'s release is unreleased.
#[test]
fn test_release_tags_on_both_sides_of_a_merge() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    setup(root);

    git(root, &["checkout", "-q", "-b", "side"]);
    change(root, "a", "side.txt", "fix: side-only change in a");
    release(
        root,
        "b",
        "chore(release): @test/b@1.1.0",
        Some("2020-01-01T00:00:00Z"),
    );

    git(root, &["checkout", "-q", "main"]);
    release(root, "a", "chore(release): @test/a@1.1.0", None);
    merge(root, "side");

    assert_eq!(next_version(root, "@test/a"), "1.1.1");
    assert_eq!(next_version(root, "@test/b"), "1.1.0");
}

/// A release tag on a commit without a conventional message still bounds the
/// package: what it released is not released again.
#[test]
fn test_tag_on_unconventional_commit_bounds_the_release() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    setup(root);

    release(root, "b", "Release b 1.1.0", None);
    change(root, "a", "fix.txt", "fix: change in a");

    assert_eq!(next_version(root, "@test/b"), "1.1.0");
    assert_eq!(next_version(root, "@test/a"), "1.0.1");
}

/// A pull request forked before the release branch released `b`: the preview
/// counts that release although the PR head is behind it.
#[test]
fn test_preview_counts_a_release_the_pr_head_is_behind() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    setup(root);

    git(root, &["checkout", "-q", "-b", "develop"]);
    change(root, "b", "develop.txt", "feat: develop change in b");
    git(root, &["checkout", "-q", "-b", "pull-request"]);
    change(root, "a", "fix.txt", "fix: change in a");

    git(root, &["checkout", "-q", "main"]);
    merge(root, "develop");
    release(root, "b", "chore(release): @test/b@1.1.0", None);
    git(root, &["checkout", "-q", "pull-request"]);

    let output = super_release_bin()
        .args(["--preview", "--base", "main", "--no-comment", "-C"])
        .arg(root)
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("`@test/a`") && stdout.contains("`1.0.0` → `1.0.1`"),
        "{stdout}"
    );
    assert!(!stdout.contains("`@test/b`"), "{stdout}");
}

/// A package's first release starts at the commit that introduced it: an
/// older commit on the branch it is merged into is not part of it.
#[test]
fn test_first_release_ignores_commits_older_than_the_package() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    setup(root);
    fs::write(
        root.join(".release.yaml"),
        format!("{CONFIG}dependencies:\n  - shared.json\n"),
    )
    .unwrap();
    git(root, &["add", "."]);
    git(
        root,
        &["commit", "-m", "chore: shared.json affects every package"],
    );

    git(root, &["checkout", "-q", "-b", "feature"]);
    git(root, &["checkout", "-q", "main"]);
    fs::write(root.join("shared.json"), "{}").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat!: change shared.json"]);

    git(root, &["checkout", "-q", "feature"]);
    fs::create_dir_all(root.join("packages/c")).unwrap();
    set_version(root, "c", "1.0.0");
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix: add c"]);
    git(root, &["checkout", "-q", "main"]);
    merge(root, "feature");

    assert_eq!(next_version(root, "@test/c"), "1.0.1");
}
