mod common;

use common::{git, super_release_bin};
use std::fs;
use tempfile::TempDir;

fn tag(root: &std::path::Path, name: &str) {
    git(root, &["tag", "-a", name, &format!("-m{}", name)]);
}

/// Helper: set up a monorepo with the given packages and dependency graph.
/// `pkgs` is a list of (name, version, dependencies_json_fragment).
fn setup_monorepo(root: &std::path::Path, pkgs: &[(&str, &str, &str)], release_yaml: &str) {
    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.email", "test@test.com"]);
    git(root, &["config", "user.name", "Test"]);

    fs::write(
        root.join("package.json"),
        r#"{"name": "mono-root", "version": "0.0.0", "private": true}"#,
    )
    .unwrap();

    for (name, version, deps) in pkgs {
        let short = name.split('/').next_back().unwrap_or(name);
        let pkg_dir = root.join(format!("packages/{}/src", short));
        fs::create_dir_all(&pkg_dir).unwrap();
        fs::write(pkg_dir.join("index.ts"), format!("// {}", name)).unwrap();

        let deps_field = if deps.is_empty() {
            String::new()
        } else {
            format!(", \"dependencies\": {{{}}}", deps)
        };
        fs::write(
            root.join(format!("packages/{}/package.json", short)),
            format!(
                r#"{{"name": "{}", "version": "{}"{}}}"#,
                name, version, deps_field
            ),
        )
        .unwrap();
    }

    fs::write(root.join(".release.yaml"), release_yaml).unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);

    for (name, version, _) in pkgs {
        tag(root, &format!("{}/v{}", name, version));
    }
}

const BASE_CONFIG: &str = r#"
branches:
  - main
exclude:
  - mono-root
steps: []
"#;

// ─── Direct dependency propagation ─────────────────────────────────────────

#[test]
fn test_dependency_change_propagates_to_dependent() {
    // A -> B (B depends on A). Change in A should release both A and B.
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    setup_monorepo(
        root,
        &[
            ("@test/core", "1.0.0", ""),
            ("@test/app", "1.0.0", r#""@test/core": "^1.0.0""#),
        ],
        BASE_CONFIG,
    );

    // Change only core
    fs::write(root.join("packages/core/src/index.ts"), "// core v2").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix: patch in core"]);

    let output = super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // core should get a direct release
    assert!(
        stdout.contains("@test/core") && stdout.contains("1.0.1"),
        "Should release @test/core 1.0.1:\n{}",
        stdout
    );
    // app should get a propagated patch release
    assert!(
        stdout.contains("@test/app") && stdout.contains("1.0.1"),
        "Should release @test/app 1.0.1 via propagation:\n{}",
        stdout
    );
    assert!(
        stdout.contains("dependency updated"),
        "Should show propagation reason:\n{}",
        stdout
    );
}

// ─── Transitive (chain) propagation ────────────────────────────────────────

#[test]
fn test_transitive_dependency_propagation() {
    // A -> B -> C (C depends on B, B depends on A).
    // Change in A should release A, B, and C.
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    setup_monorepo(
        root,
        &[
            ("@test/core", "1.0.0", ""),
            ("@test/mid", "1.0.0", r#""@test/core": "^1.0.0""#),
            ("@test/app", "1.0.0", r#""@test/mid": "^1.0.0""#),
        ],
        BASE_CONFIG,
    );

    // Change only core
    fs::write(root.join("packages/core/src/index.ts"), "// core v2").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat: new feature in core"]);

    let output = super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        stdout.contains("@test/core") && stdout.contains("1.1.0"),
        "Should release @test/core 1.1.0 (feat):\n{}",
        stdout
    );
    assert!(
        stdout.contains("@test/mid") && stdout.contains("1.0.1"),
        "Should release @test/mid 1.0.1 (propagated from core):\n{}",
        stdout
    );
    assert!(
        stdout.contains("@test/app") && stdout.contains("1.0.1"),
        "Should release @test/app 1.0.1 (transitively propagated):\n{}",
        stdout
    );
    // 3 packages total
    assert!(
        stdout.contains("3 package(s) to release"),
        "Should plan 3 releases:\n{}",
        stdout
    );
}

// ─── No propagation when no dependency relationship ────────────────────────

#[test]
fn test_no_propagation_without_dependency() {
    // A and B are independent. Change in A should NOT release B.
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    setup_monorepo(
        root,
        &[("@test/core", "1.0.0", ""), ("@test/utils", "1.0.0", "")],
        BASE_CONFIG,
    );

    fs::write(root.join("packages/core/src/index.ts"), "// core v2").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix: patch in core"]);

    let output = super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());

    assert!(
        stdout.contains("@test/core") && stdout.contains("1.0.1"),
        "Should release @test/core:\n{}",
        stdout
    );
    assert!(
        stdout.contains("1 package(s) to release"),
        "Should plan only 1 release (utils has no dependency on core):\n{}",
        stdout
    );
}

// ─── Propagation with optionalDependencies ─────────────────────────────────

#[test]
fn test_optional_dependency_propagation() {
    // Root package has optionalDependencies on platform packages.
    // Change in platform package should propagate to root.
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.email", "test@test.com"]);
    git(root, &["config", "user.name", "Test"]);

    // Root package with optionalDependencies
    fs::write(
        root.join("package.json"),
        r#"{"name": "my-tool", "version": "1.0.0", "optionalDependencies": {"my-tool-linux-x64": "^1.0.0"}}"#,
    )
    .unwrap();
    fs::write(root.join("index.js"), "// root").unwrap();

    fs::create_dir_all(root.join("packages/linux-x64")).unwrap();
    fs::write(
        root.join("packages/linux-x64/package.json"),
        r#"{"name": "my-tool-linux-x64", "version": "1.0.0"}"#,
    )
    .unwrap();
    fs::write(root.join("packages/linux-x64/bin"), "binary").unwrap();

    fs::write(
        root.join(".release.yaml"),
        r#"
branches:
  - main
steps: []
"#,
    )
    .unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    tag(root, "v1.0.0");
    tag(root, "my-tool-linux-x64/v1.0.0");

    // Change only the platform package
    fs::write(root.join("packages/linux-x64/bin"), "updated binary").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix: update linux binary"]);

    let output = super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        stdout.contains("my-tool-linux-x64") && stdout.contains("1.0.1"),
        "Should release my-tool-linux-x64 1.0.1:\n{}",
        stdout
    );
    assert!(
        stdout.contains("my-tool ") || stdout.contains("my-tool\x1b"),
        "Should release my-tool (propagated from optional dep):\n{}",
        stdout
    );
    assert!(
        stdout.contains("dependency updated"),
        "Should show propagation reason:\n{}",
        stdout
    );
}

// ─── Circular dependency handling ──────────────────────────────────────────

#[test]
fn test_circular_dependency_no_infinite_loop() {
    // A depends on B, B depends on A (circular). Should not hang or crash.
    // Note: topological_sort would reject this, but propagation should handle it gracefully.
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.email", "test@test.com"]);
    git(root, &["config", "user.name", "Test"]);

    fs::write(
        root.join("package.json"),
        r#"{"name": "mono-root", "version": "0.0.0", "private": true}"#,
    )
    .unwrap();

    fs::create_dir_all(root.join("packages/a/src")).unwrap();
    fs::write(
        root.join("packages/a/package.json"),
        r#"{"name": "@test/a", "version": "1.0.0", "dependencies": {"@test/b": "^1.0.0"}}"#,
    )
    .unwrap();
    fs::write(root.join("packages/a/src/index.ts"), "// a").unwrap();

    fs::create_dir_all(root.join("packages/b/src")).unwrap();
    fs::write(
        root.join("packages/b/package.json"),
        r#"{"name": "@test/b", "version": "1.0.0", "dependencies": {"@test/a": "^1.0.0"}}"#,
    )
    .unwrap();
    fs::write(root.join("packages/b/src/index.ts"), "// b").unwrap();

    fs::write(
        root.join(".release.yaml"),
        "branches:\n  - main\nexclude:\n  - mono-root\nsteps: []\n",
    )
    .unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    tag(root, "@test/a/v1.0.0");
    tag(root, "@test/b/v1.0.0");

    // Change only A
    fs::write(root.join("packages/a/src/index.ts"), "// a v2").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix: patch in a"]);

    let output = super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Should not hang and should succeed (or fail with circular dep error — either is fine)
    // The key assertion: the process terminates
    assert!(
        output.status.success(),
        "Should handle circular deps gracefully:\nstdout: {}\nstderr: {}",
        stdout,
        String::from_utf8_lossy(&output.stderr)
    );

    // Both should be released: A directly, B via propagation
    assert!(
        stdout.contains("@test/a") && stdout.contains("1.0.1"),
        "Should release @test/a:\n{}",
        stdout
    );
    assert!(
        stdout.contains("@test/b") && stdout.contains("1.0.1"),
        "Should release @test/b (propagated):\n{}",
        stdout
    );
}

// ─── Already-released dependent is not double-released ─────────────────────

#[test]
fn test_no_double_release_when_both_changed() {
    // A -> B. Both A and B have direct commits. B should get its own bump, not a propagated patch.
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    setup_monorepo(
        root,
        &[
            ("@test/core", "1.0.0", ""),
            ("@test/app", "1.0.0", r#""@test/core": "^1.0.0""#),
        ],
        BASE_CONFIG,
    );

    // Change both packages
    fs::write(root.join("packages/core/src/index.ts"), "// core v2").unwrap();
    fs::write(root.join("packages/app/src/index.ts"), "// app v2").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat: update both core and app"]);

    let output = super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());

    // Both get minor (feat), not a propagated patch for app
    assert!(
        stdout.contains("@test/core") && stdout.contains("1.1.0"),
        "core should be 1.1.0:\n{}",
        stdout
    );
    assert!(
        stdout.contains("@test/app") && stdout.contains("1.1.0"),
        "app should be 1.1.0 from its own feat commit, not 1.0.1 propagated:\n{}",
        stdout
    );
    // Should NOT show "dependency updated" for app since it has its own commits
    assert!(
        stdout.matches("dependency updated").count() == 0,
        "app should not be marked as propagated when it has direct changes:\n{}",
        stdout
    );
    assert!(
        stdout.contains("2 package(s) to release"),
        "Should plan exactly 2 releases:\n{}",
        stdout
    );
}

// ─── Diamond dependency propagation ────────────────────────────────────────

#[test]
fn test_diamond_dependency_propagation() {
    // Diamond: A -> B, A -> C, B -> D, C -> D
    // Change in D should propagate to B, C, and A.
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    setup_monorepo(
        root,
        &[
            ("@test/d", "1.0.0", ""),
            ("@test/b", "1.0.0", r#""@test/d": "^1.0.0""#),
            ("@test/c", "1.0.0", r#""@test/d": "^1.0.0""#),
            (
                "@test/a",
                "1.0.0",
                r#""@test/b": "^1.0.0", "@test/c": "^1.0.0""#,
            ),
        ],
        BASE_CONFIG,
    );

    // Change only D
    fs::write(root.join("packages/d/src/index.ts"), "// d v2").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix: patch in d"]);

    let output = super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        stdout.contains("4 package(s) to release"),
        "All 4 packages should be released:\n{}",
        stdout
    );
    for name in &["@test/a", "@test/b", "@test/c", "@test/d"] {
        assert!(
            stdout.contains(name),
            "Should release {}:\n{}",
            name,
            stdout
        );
    }
}

// ─── Propagation on prerelease branches ────────────────────────────────────

const PRERELEASE_CONFIG: &str = r#"
branches:
  - main
  - name: "test-*"
    prerelease: true
exclude:
  - mono-root
steps: []
"#;

#[test]
fn test_prerelease_propagation_from_stable_base() {
    // On a prerelease branch, a propagated dependent must get a prerelease
    // version, never a stable one.
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    setup_monorepo(
        root,
        &[
            ("@test/core", "1.0.0", ""),
            ("@test/app", "1.0.0", r#""@test/core": "^1.0.0""#),
        ],
        PRERELEASE_CONFIG,
    );

    git(root, &["checkout", "-b", "test-branch"]);
    fs::write(root.join("packages/core/src/index.ts"), "// core v2").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix: patch in core"]);

    let output = super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Exact per-package versions via the uncolored version-bump lines.
    assert!(
        stdout.contains("packages/core/package.json: 1.0.0 -> 1.0.1-test-branch.1"),
        "Should release @test/core 1.0.1-test-branch.1:\n{}",
        stdout
    );
    assert!(
        stdout.contains("packages/app/package.json: 1.0.0 -> 1.0.1-test-branch.1"),
        "Propagated @test/app must stay on the channel:\n{}",
        stdout
    );
    assert!(
        stdout.contains("dependency updated"),
        "Should show propagation reason:\n{}",
        stdout
    );
}

#[test]
fn test_prerelease_propagation_increments_existing_channel() {
    // Regression test: a dependent already on the channel
    // (10.267.0-test-tasks-tsmain-2.1) was propagated to a stable 10.267.1
    // instead of incrementing the prerelease number.
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    setup_monorepo(
        root,
        &[
            ("@test/core", "1.0.0", ""),
            ("@test/app", "1.0.0", r#""@test/core": "^1.0.0""#),
        ],
        PRERELEASE_CONFIG,
    );

    git(root, &["checkout", "-b", "test-branch"]);
    // Both packages already have a release on this channel (semver-greater
    // than the stable 1.0.0 so it is picked as the base version).
    tag(root, "@test/core/v1.1.0-test-branch.1");
    tag(root, "@test/app/v1.1.0-test-branch.1");

    fs::write(root.join("packages/core/src/index.ts"), "// core v2").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix: another fix in core"]);

    let output = super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        stdout.contains("packages/core/package.json: 1.1.0-test-branch.1 -> 1.1.0-test-branch.2"),
        "Should release @test/core 1.1.0-test-branch.2:\n{}",
        stdout
    );
    assert!(
        stdout.contains("packages/app/package.json: 1.1.0-test-branch.1 -> 1.1.0-test-branch.2"),
        "Propagated @test/app must increment on the channel:\n{}",
        stdout
    );
    assert!(
        stdout.contains("dependency updated"),
        "Should show propagation reason:\n{}",
        stdout
    );
    assert!(
        stdout.contains("2 package(s) to release"),
        "Should plan 2 releases:\n{}",
        stdout
    );
    assert!(
        !stdout.contains("1.1.1"),
        "Propagated dependent must not get a stable bump on a prerelease branch:\n{}",
        stdout
    );
}

#[test]
fn test_prerelease_transitive_propagation() {
    // A -> B -> C chain on a prerelease branch: every propagated release
    // must carry the channel prerelease.
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    setup_monorepo(
        root,
        &[
            ("@test/core", "1.0.0", ""),
            ("@test/mid", "1.0.0", r#""@test/core": "^1.0.0""#),
            ("@test/app", "1.0.0", r#""@test/mid": "^1.0.0""#),
        ],
        PRERELEASE_CONFIG,
    );

    git(root, &["checkout", "-b", "test-branch"]);
    fs::write(root.join("packages/core/src/index.ts"), "// core v2").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix: patch in core"]);

    let output = super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        stdout.contains("3 package(s) to release"),
        "Should plan 3 releases:\n{}",
        stdout
    );
    // All three packages start from stable 1.0.0, so all must land on the
    // channel's first prerelease of the patch bump.
    for short in ["core", "mid", "app"] {
        assert!(
            stdout.contains(&format!(
                "packages/{}/package.json: 1.0.0 -> 1.0.1-test-branch.1",
                short
            )),
            "packages/{} must land on the channel:\n{}",
            short,
            stdout
        );
    }
}

#[test]
fn test_propagation_collision_skips_dependent_with_warning() {
    // A propagated version already tagged on another branch must not abort
    // the run: the dependent is skipped with a warning (also in dry runs).
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    setup_monorepo(
        root,
        &[
            ("@test/core", "1.0.0", ""),
            ("@test/app", "1.0.0", r#""@test/core": "^1.0.0""#),
        ],
        BASE_CONFIG,
    );

    // Tag app's next patch version on a side branch so the tag exists but is
    // unreachable from main.
    git(root, &["checkout", "-b", "other"]);
    fs::write(root.join("packages/app/src/index.ts"), "// other").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix: change on other branch"]);
    tag(root, "@test/app/v1.0.1");
    git(root, &["checkout", "main"]);

    fs::write(root.join("packages/core/src/index.ts"), "// core v2").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix: patch in core"]);

    let output = super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {}", stderr);

    assert!(
        stdout.contains("packages/core/package.json: 1.0.0 -> 1.0.1"),
        "Core's own release must survive the dependent's collision:\n{}",
        stdout
    );
    assert!(
        stdout.contains("1 package(s) to release"),
        "The colliding dependent must be skipped, not released:\n{}",
        stdout
    );
    assert!(
        stderr.contains("Skipping cascade to '@test/app'"),
        "Should warn about the skipped dependent:\n{}",
        stderr
    );
}

#[test]
fn test_propagation_collision_detected_despite_build_metadata_tag() {
    // Same as above, but the colliding tag carries build metadata:
    // v1.0.1+ci.7 and v1.0.1 name the same release number.
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    setup_monorepo(
        root,
        &[
            ("@test/core", "1.0.0", ""),
            ("@test/app", "1.0.0", r#""@test/core": "^1.0.0""#),
        ],
        BASE_CONFIG,
    );

    git(root, &["checkout", "-b", "other"]);
    fs::write(root.join("packages/app/src/index.ts"), "// other").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix: change on other branch"]);
    tag(root, "@test/app/v1.0.1+ci.7");
    git(root, &["checkout", "main"]);

    fs::write(root.join("packages/core/src/index.ts"), "// core v2").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix: patch in core"]);

    let output = super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "stderr: {}", stderr);

    assert!(
        stdout.contains("1 package(s) to release"),
        "The colliding dependent must be skipped:\n{}",
        stdout
    );
    assert!(
        stderr.contains("Skipping cascade to '@test/app'"),
        "Build metadata must not hide the collision:\n{}",
        stderr
    );
}

#[test]
fn test_prerelease_stale_channel_tag_fails_with_guidance() {
    // A channel tag that exists but is unreachable from HEAD (rebase or
    // force-push) would silently produce an already-released version — the
    // run must fail and tell the user how to resolve it.
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    setup_monorepo(
        root,
        &[
            ("@test/core", "1.0.0", ""),
            ("@test/app", "1.0.0", r#""@test/core": "^1.0.0""#),
        ],
        PRERELEASE_CONFIG,
    );

    // Simulate a pre-rebase release: the channel tag sits on a commit that is
    // no longer reachable from the branch.
    git(root, &["checkout", "-b", "old-state"]);
    fs::write(root.join("packages/app/src/index.ts"), "// pre-rebase").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix: pre-rebase change"]);
    tag(root, "@test/app/v1.0.1-test-branch.1");
    git(root, &["checkout", "main"]);

    git(root, &["checkout", "-b", "test-branch"]);
    fs::write(root.join("packages/core/src/index.ts"), "// core v2").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix: patch in core"]);

    let output = super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "Run must fail on a stale channel tag; stdout:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        stderr.contains("already exists as a tag but is not reachable"),
        "Should explain the stale-tag situation:\n{}",
        stderr
    );
}

const PRERELEASE_ALLOWLIST_CONFIG: &str = r#"
branches:
  - main
  - name: "test-*"
    prerelease: true
    packages: ["@test/core"]
exclude:
  - mono-root
steps: []
"#;

#[test]
fn test_prerelease_allowlist_excluded_stale_tag_does_not_block_run() {
    // A stale channel tag on a package the branch's `packages:` allowlist
    // excludes must not abort the run — that package is never released here.
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    setup_monorepo(
        root,
        &[("@test/core", "1.0.0", ""), ("@test/app", "1.0.0", "")],
        PRERELEASE_ALLOWLIST_CONFIG,
    );

    git(root, &["checkout", "-b", "old-state"]);
    fs::write(root.join("packages/app/src/index.ts"), "// pre-rebase").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix: pre-rebase change"]);
    tag(root, "@test/app/v1.0.1-test-branch.1");
    git(root, &["checkout", "main"]);

    git(root, &["checkout", "-b", "test-branch"]);
    fs::write(root.join("packages/core/src/index.ts"), "// core v2").unwrap();
    fs::write(root.join("packages/app/src/index.ts"), "// app v2").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix: change both packages"]);

    let output = super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "Excluded package's stale tag must not fail the run; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("packages/core/package.json: 1.0.0 -> 1.0.1-test-branch.1"),
        "Allowlisted package must still release:\n{}",
        stdout
    );
    assert!(
        !stdout.contains("packages/app/package.json:"),
        "Excluded package must not be released:\n{}",
        stdout
    );
}

#[test]
fn test_preview_warns_instead_of_failing_on_unreachable_channel_tag() {
    // Previews check out PR heads, where the latest channel tag is routinely
    // not reachable — the stale-tag protection must warn, not abort.
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    setup_monorepo(root, &[("@test/core", "1.0.0", "")], PRERELEASE_CONFIG);

    git(root, &["checkout", "-b", "test-branch"]);
    fs::write(root.join("packages/core/src/index.ts"), "// first").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix: first"]);
    tag(root, "@test/core/v1.0.1-test-branch.1");

    // PR branch forks here; the release branch then moves on with another release.
    git(root, &["checkout", "-b", "feature-pr"]);
    git(root, &["checkout", "test-branch"]);
    fs::write(root.join("packages/core/src/index.ts"), "// second").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix: second"]);
    tag(root, "@test/core/v1.0.1-test-branch.2");

    git(root, &["checkout", "feature-pr"]);
    fs::write(root.join("packages/core/src/index.ts"), "// pr change").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix: pr change"]);

    let output = super_release_bin()
        .args(["--preview", "--base", "test-branch", "--no-comment"])
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "Preview must not fail on an unreachable channel tag; stderr: {}",
        stderr
    );
    assert!(
        stderr.contains("already exists as a tag not"),
        "Preview should warn about the unreachable tag:\n{}",
        stderr
    );
}

#[test]
fn test_stable_branch_strips_manifest_prerelease_on_first_release() {
    // A first-release package whose manifest carries a prerelease (1.0.0-rc.1)
    // must not ship a prerelease from a stable branch — the base is treated
    // as 1.0.0, and direct and propagated releases agree.
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    setup_monorepo(
        root,
        &[
            ("@test/core", "1.0.0-rc.1", ""),
            ("@test/app", "1.0.0", r#""@test/core": "^1.0.0""#),
        ],
        BASE_CONFIG,
    );

    fs::write(root.join("packages/core/src/index.ts"), "// core v2").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix: patch in core"]);

    let output = super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(
        stdout.contains("packages/core/package.json: 1.0.0 -> 1.0.1"),
        "Manifest prerelease must be stripped to a stable base:\n{}",
        stdout
    );
    assert!(
        stdout.contains("packages/app/package.json: 1.0.0 -> 1.0.1"),
        "Propagated dependent must agree with the direct path:\n{}",
        stdout
    );
    assert!(
        !stdout.contains("rc."),
        "No prerelease may appear in a stable branch's plan:\n{}",
        stdout
    );
}
