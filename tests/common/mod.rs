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
    Command::cargo_bin("super-release").unwrap()
}
