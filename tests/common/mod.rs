use assert_cmd::Command;
use std::path::Path;
use std::process;

pub fn git(dir: &Path, args: &[&str]) {
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

pub fn super_release_bin() -> Command {
    let mut cmd = Command::cargo_bin("super-release").unwrap();
    // Keep output free of CI log-group markers when the tests themselves run in CI.
    cmd.env_remove("GITHUB_ACTIONS")
        .env_remove("GITLAB_CI")
        .env_remove("TF_BUILD");
    cmd
}
