mod common;

use common::{git, super_release_bin};
use std::fs;
use tempfile::TempDir;

/// Helper: set up a monorepo with two packages and git init.
/// Returns after the initial commit (nothing tagged yet).
fn setup_monorepo(root: &std::path::Path, core_version: &str, utils_version: &str) {
    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.email", "test@test.com"]);
    git(root, &["config", "user.name", "Test"]);

    fs::write(
        root.join("package.json"),
        r#"{"name": "monorepo", "version": "0.0.0", "private": true}"#,
    )
    .unwrap();

    fs::create_dir_all(root.join("packages/core/src")).unwrap();
    fs::write(
        root.join("packages/core/package.json"),
        format!(r#"{{"name": "@acme/core", "version": "{}"}}"#, core_version),
    )
    .unwrap();
    fs::write(root.join("packages/core/src/index.ts"), "// init").unwrap();

    fs::create_dir_all(root.join("packages/utils/src")).unwrap();
    fs::write(
        root.join("packages/utils/package.json"),
        format!(
            r#"{{"name": "@acme/utils", "version": "{}"}}"#,
            utils_version
        ),
    )
    .unwrap();
    fs::write(root.join("packages/utils/src/index.ts"), "// init").unwrap();
}

fn tag(root: &std::path::Path, name: &str) {
    git(root, &["tag", "-a", name, &format!("-m{}", name)]);
}

#[test]
fn test_monorepo_maintenance_independent_patch_skips_out_of_range() {
    // Scenario 1: @acme/core at 1.4.0, @acme/utils at 2.0.0.
    // Branch 1.4.x: fix should produce core 1.4.1, utils skipped (out of range).
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    setup_monorepo(root, "1.0.0", "1.0.0");

    fs::write(
        root.join(".release.yaml"),
        r#"
branches:
  - main
  - name: "*.*.x"
    maintenance: true
exclude:
  - monorepo
steps: []
"#,
    )
    .unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    tag(root, "@acme/core/v1.0.0");
    tag(root, "@acme/utils/v1.0.0");

    // Evolve core to 1.4.0 on main
    for minor in 1..=4 {
        fs::write(
            root.join("packages/core/src/index.ts"),
            format!("// v1.{}", minor),
        )
        .unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-m", "feat: core feature"]);
        tag(root, &format!("@acme/core/v1.{}.0", minor));
    }

    // Evolve utils to 2.0.0 on main
    fs::write(root.join("packages/utils/src/index.ts"), "// v2").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat!: utils breaking change"]);
    tag(root, "@acme/utils/v2.0.0");

    // Continue core to 1.6.0
    for minor in 5..=6 {
        fs::write(
            root.join("packages/core/src/index.ts"),
            format!("// v1.{}", minor),
        )
        .unwrap();
        git(root, &["add", "."]);
        git(root, &["commit", "-m", "feat: core feature"]);
        tag(root, &format!("@acme/core/v1.{}.0", minor));
    }

    // Create maintenance branch from core v1.4.0
    git(root, &["checkout", "@acme/core/v1.4.0", "-b", "1.4.x"]);

    // Fix in core
    fs::write(root.join("packages/core/src/index.ts"), "// v1.4 hotfix").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix: security patch in core"]);

    let output = super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "Should succeed:\nstdout: {}\nstderr: {}",
        stdout,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("@acme/core") && stdout.contains("1.4.1"),
        "Should release @acme/core 1.4.1:\n{}",
        stdout
    );
    // utils at 2.0.0 is outside 1.4.x range — must not appear in release plan
    assert!(
        !stdout.contains("@acme/utils/v"),
        "Should not release @acme/utils (out of 1.4.x range):\n{}",
        stdout
    );
}

#[test]
fn test_monorepo_maintenance_per_package_branches_with_filter() {
    // Scenario 2: Two separate maintenance branches, each filtered to one package.
    // core-1.x → only @acme/core, utils-1.x → only @acme/utils.
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    setup_monorepo(root, "1.3.0", "1.5.0");

    fs::write(
        root.join(".release.yaml"),
        r#"
branches:
  - main
  - name: "core-1.x"
    maintenance: true
    range: "1.x"
    packages:
      - "@acme/core"
  - name: "utils-1.x"
    maintenance: true
    range: "1.x"
    packages:
      - "@acme/utils"
exclude:
  - monorepo
steps: []
"#,
    )
    .unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    tag(root, "@acme/core/v1.3.0");
    tag(root, "@acme/utils/v1.5.0");

    // ---- Test core-1.x branch ----
    git(root, &["checkout", "-b", "core-1.x"]);
    fs::write(root.join("packages/core/src/index.ts"), "// core fix").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix: core bugfix"]);

    let output = super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "core-1.x should succeed:\nstdout: {}\nstderr: {}",
        stdout,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("@acme/core") && stdout.contains("1.3.1"),
        "Should release @acme/core 1.3.1:\n{}",
        stdout
    );
    // utils must not appear in the release plan
    assert!(
        !stdout.contains("@acme/utils/v"),
        "Should not release @acme/utils on core-1.x:\n{}",
        stdout
    );

    // ---- Test utils-1.x branch ----
    git(root, &["checkout", "main"]);
    git(root, &["checkout", "-b", "utils-1.x"]);
    fs::write(root.join("packages/utils/src/index.ts"), "// utils fix").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix: utils bugfix"]);

    let output = super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "utils-1.x should succeed:\nstdout: {}\nstderr: {}",
        stdout,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("@acme/utils") && stdout.contains("1.5.1"),
        "Should release @acme/utils 1.5.1:\n{}",
        stdout
    );
    // core must not appear in the release plan
    assert!(
        !stdout.contains("@acme/core/v"),
        "Should not release @acme/core on utils-1.x:\n{}",
        stdout
    );
}

#[test]
fn test_monorepo_maintenance_cascade_to_dependent() {
    // Scenario 3: @acme/app depends on @acme/lib. Fix in lib should cascade to app
    // because app declares lib as a dependency.
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    git(root, &["init", "-b", "main"]);
    git(root, &["config", "user.email", "test@test.com"]);
    git(root, &["config", "user.name", "Test"]);

    fs::write(
        root.join("package.json"),
        r#"{"name": "monorepo", "version": "0.0.0", "private": true}"#,
    )
    .unwrap();

    fs::create_dir_all(root.join("packages/lib/src")).unwrap();
    fs::write(
        root.join("packages/lib/package.json"),
        r#"{"name": "@acme/lib", "version": "1.0.0"}"#,
    )
    .unwrap();
    fs::write(root.join("packages/lib/src/index.ts"), "// lib").unwrap();

    fs::create_dir_all(root.join("packages/app/src")).unwrap();
    fs::write(
        root.join("packages/app/package.json"),
        r#"{"name": "@acme/app", "version": "1.0.0", "dependencies": {"@acme/lib": "^1.0.0"}}"#,
    )
    .unwrap();
    fs::write(root.join("packages/app/src/index.ts"), "// app").unwrap();

    fs::write(
        root.join(".release.yaml"),
        r#"
branches:
  - main
  - name: "1.x"
    maintenance: true
exclude:
  - monorepo
steps: []
"#,
    )
    .unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    tag(root, "@acme/lib/v1.0.0");
    tag(root, "@acme/app/v1.0.0");

    // Maintenance branch
    git(root, &["checkout", "-b", "1.x"]);

    // Fix ONLY in lib — don't touch app
    fs::write(root.join("packages/lib/src/index.ts"), "// lib fix").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix: patch in lib only"]);

    let output = super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "Should succeed:\nstdout: {}\nstderr: {}",
        stdout,
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("@acme/lib") && stdout.contains("1.0.1"),
        "Should release @acme/lib 1.0.1:\n{}",
        stdout
    );
    // app SHOULD be released as a patch because its dependency @acme/lib changed
    assert!(
        stdout.contains("@acme/app") && stdout.contains("1.0.1"),
        "Should release @acme/app 1.0.1 (dependency propagation):\n{}",
        stdout
    );
    assert!(
        stdout.contains("dependency updated"),
        "Should indicate the release was propagated from a dependency:\n{}",
        stdout
    );
}

#[test]
fn test_monorepo_maintenance_simultaneous_independent_bumps() {
    // Scenario 4: Two packages at different versions both get fixes on same 1.x branch.
    // Each should get its own independent patch bump.
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    setup_monorepo(root, "1.2.0", "1.5.0");

    fs::write(
        root.join(".release.yaml"),
        r#"
branches:
  - main
  - name: "1.x"
    maintenance: true
exclude:
  - monorepo
steps: []
"#,
    )
    .unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    tag(root, "@acme/core/v1.2.0");
    tag(root, "@acme/utils/v1.5.0");

    // Maintenance branch
    git(root, &["checkout", "-b", "1.x"]);

    // Fix in core
    fs::write(root.join("packages/core/src/index.ts"), "// core fix").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix: core patch"]);

    // Fix in utils (separate commit)
    fs::write(root.join("packages/utils/src/index.ts"), "// utils fix").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix: utils patch"]);

    let output = super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "Should succeed:\nstdout: {}\nstderr: {}",
        stdout,
        String::from_utf8_lossy(&output.stderr)
    );
    // Each package gets its own version bump
    assert!(
        stdout.contains("@acme/core") && stdout.contains("1.2.1"),
        "Should release @acme/core 1.2.1:\n{}",
        stdout
    );
    assert!(
        stdout.contains("@acme/utils") && stdout.contains("1.5.1"),
        "Should release @acme/utils 1.5.1:\n{}",
        stdout
    );
}

#[test]
fn test_monorepo_maintenance_breaking_capped_independently() {
    // Scenario 5: On 1.x branch, one package gets feat!, the other gets fix.
    // Breaking change should be capped to minor for that package only.
    let dir = TempDir::new().unwrap();
    let root = dir.path();

    setup_monorepo(root, "1.2.0", "1.5.0");

    fs::write(
        root.join(".release.yaml"),
        r#"
branches:
  - main
  - name: "1.x"
    maintenance: true
exclude:
  - monorepo
steps: []
"#,
    )
    .unwrap();

    git(root, &["add", "."]);
    git(root, &["commit", "-m", "chore: init"]);
    tag(root, "@acme/core/v1.2.0");
    tag(root, "@acme/utils/v1.5.0");

    // Maintenance branch
    git(root, &["checkout", "-b", "1.x"]);

    // Breaking change in core only
    fs::write(
        root.join("packages/core/src/index.ts"),
        "// breaking core change",
    )
    .unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "feat!: breaking core API"]);

    // Normal fix in utils only
    fs::write(root.join("packages/utils/src/index.ts"), "// utils bugfix").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-m", "fix: utils bugfix"]);

    let output = super_release_bin()
        .arg("--dry-run")
        .arg("-C")
        .arg(root.to_str().unwrap())
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "Should succeed:\nstdout: {}\nstderr: {}",
        stdout,
        String::from_utf8_lossy(&output.stderr)
    );
    // Core: breaking capped to minor on 1.x → 1.3.0
    assert!(
        stdout.contains("@acme/core") && stdout.contains("1.3.0"),
        "Should release @acme/core 1.3.0 (breaking capped to minor):\n{}",
        stdout
    );
    assert!(
        !stdout.contains("2.0.0"),
        "Should NOT bump core to 2.0.0 on maintenance branch:\n{}",
        stdout
    );
    // Utils: normal patch → 1.5.1
    assert!(
        stdout.contains("@acme/utils") && stdout.contains("1.5.1"),
        "Should release @acme/utils 1.5.1 (normal patch):\n{}",
        stdout
    );
}
