use anyhow::{Context, Result};
use console::style;
use indicatif::{ProgressBar, ProgressStyle};
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::time::Duration;

/// Format a Command for display (program + args).
pub fn format_command(cmd: &Command) -> String {
    let prog = cmd.get_program().to_string_lossy().to_string();
    let args: Vec<String> = cmd
        .get_args()
        .map(|a| {
            let s = a.to_string_lossy();
            if s.contains(' ') {
                format!("\"{}\"", s)
            } else {
                s.to_string()
            }
        })
        .collect();
    if args.is_empty() {
        prog
    } else {
        format!("{} {}", prog, args.join(" "))
    }
}

/// Run a command with output handling:
/// - TTY: shows a spinner with the last output line
/// - On success: prints last 3 lines as summary
/// - On error: dumps last 20 lines for debugging
/// - Detects "already published" errors and treats them as success
pub fn run_command(
    mut cmd: Command,
    label: &str,
    plugin_name: &str,
) -> Result<()> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .with_context(|| format!("Failed to run {} publish for {}", plugin_name, label))?;

    let is_tty = console::Term::stdout().is_term();

    let spinner = if is_tty {
        let s = ProgressBar::new_spinner();
        s.set_style(
            ProgressStyle::default_spinner()
                .template("  {spinner:.cyan} [{msg}] Publishing {prefix}...")
                .unwrap(),
        );
        s.set_prefix(label.to_string());
        s.set_message("starting");
        s.enable_steady_tick(Duration::from_millis(80));
        Some(s)
    } else {
        None
    };

    // Read stdout and stderr on separate threads to avoid pipe deadlock
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let stdout_handle = std::thread::spawn(move || collect_lines(stdout));
    let stderr_lines = collect_lines(stderr);
    let stdout_lines = stdout_handle.join().unwrap_or_default();

    let mut all_output = stdout_lines;
    all_output.extend(stderr_lines);

    if let Some(ref s) = spinner {
        if let Some(last) = all_output.last() {
            s.set_message(truncate(last, 60));
        }
        s.finish_and_clear();
    }

    let status = child.wait()?;

    if !status.success() {
        let full_output = all_output.join("\n");

        if is_already_published(&full_output) {
            println!("  [{}] {} already published, skipping", plugin_name, label);
            return Ok(());
        }

        let tail: Vec<&str> = all_output.iter().map(|s| s.as_str()).rev().take(20).collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
        eprintln!(
            "  [{}] {} publish output:\n{}",
            style(plugin_name).red(),
            label,
            tail.iter()
                .map(|l| format!("    {}", style(l).dim()))
                .collect::<Vec<_>>()
                .join("\n")
        );

        anyhow::bail!(
            "{} publish failed for {} (exit code: {})",
            plugin_name,
            label,
            status
        );
    }

    let summary_lines: Vec<&str> = all_output
        .iter()
        .map(|s| s.as_str())
        .filter(|s| !s.trim().is_empty())
        .rev()
        .take(3)
        .collect();

    println!("  [{}] Published {}", plugin_name, label);
    for line in summary_lines.into_iter().rev() {
        println!("    {}", style(line).dim());
    }

    Ok(())
}

fn collect_lines<R: std::io::Read>(reader: Option<R>) -> Vec<String> {
    let Some(r) = reader else {
        return Vec::new();
    };
    BufReader::new(r)
        .lines()
        .map(|l| l.unwrap_or_default())
        .collect()
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max - 3).collect();
        format!("{}...", truncated)
    }
}

/// Detect "version already exists" errors from npm/yarn/pnpm.
fn is_already_published(output: &str) -> bool {
    let patterns = [
        "previously published version",
        "EPUBLISHCONFLICT",
        "already been published",
        "cannot publish over",
        "Version already exists",
    ];
    patterns.iter().any(|p| output.contains(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_already_published_detection() {
        assert!(is_already_published(
            "npm ERR! 403 You cannot publish over the previously published versions: 1.0.0"
        ));
        assert!(is_already_published("npm error code EPUBLISHCONFLICT"));
        assert!(is_already_published("This package has already been published"));
        assert!(is_already_published("Version already exists"));
        assert!(!is_already_published("npm ERR! 403 Forbidden"));
        assert!(!is_already_published("network timeout"));
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("short", 60), "short");
        assert_eq!(truncate("a".repeat(100).as_str(), 20), format!("{}...", "a".repeat(17)));
    }
}
