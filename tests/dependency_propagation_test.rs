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
        let short = name.split('/').last().unwrap_or(name);
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
            (
                "@test/app",
                "1.0.0",
                r#""@test/core": "^1.0.0""#,
            ),
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
        &[
            ("@test/core", "1.0.0", ""),
            ("@test/utils", "1.0.0", ""),
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
            (
                "@test/app",
                "1.0.0",
                r#""@test/core": "^1.0.0""#,
            ),
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
