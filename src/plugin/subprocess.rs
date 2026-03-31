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

/// Options for `run_command`.
pub struct RunOptions<'a> {
    /// Label shown in output (e.g. "@acme/core v1.1.0")
    pub label: &'a str,
    /// Plugin/context name shown in brackets (e.g. "npm", "exec:prepare")
    pub plugin_name: &'a str,
    /// Optional function to check if a failure is recoverable.
    /// If it returns true for the combined output, the error is suppressed.
    pub is_recoverable: Option<fn(&str) -> bool>,
}

/// Run a command with output handling:
/// - TTY: shows a spinner with the last output line
/// - On success: prints last 3 lines as summary
/// - On error: dumps last 20 lines for debugging
/// - If `is_recoverable` returns true, treats the error as success
pub fn run_command(mut cmd: Command, opts: &RunOptions) -> Result<()> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = cmd.spawn().with_context(|| {
        format!(
            "[{}] Failed to spawn command for {}",
            opts.plugin_name, opts.label
        )
    })?;

    let is_tty = console::Term::stdout().is_term();

    let spinner = if is_tty {
        let s = ProgressBar::new_spinner();
        s.set_style(
            ProgressStyle::default_spinner()
                .template("  {spinner:.cyan} [{prefix}] {msg}")
                .unwrap(),
        );
        s.set_prefix(opts.plugin_name.to_string());
        s.set_message(format!("Running {}...", opts.label));
        s.enable_steady_tick(Duration::from_millis(80));
        Some(s)
    } else {
        None
    };

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

        if let Some(check) = opts.is_recoverable
            && check(&full_output)
        {
            println!(
                "  [{}] {} — recoverable, skipping",
                opts.plugin_name, opts.label
            );
            return Ok(());
        }

        let tail: Vec<&str> = all_output
            .iter()
            .map(|s| s.as_str())
            .rev()
            .take(20)
            .collect();
        let tail: Vec<&str> = tail.into_iter().rev().collect();
        eprintln!(
            "  [{}] {} output:\n{}",
            style(opts.plugin_name).red(),
            opts.label,
            tail.iter()
                .map(|l| format!("    {}", style(l).dim()))
                .collect::<Vec<_>>()
                .join("\n")
        );

        anyhow::bail!(
            "[{}] Command failed for {} (exit code: {})",
            opts.plugin_name,
            opts.label,
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

    if !summary_lines.is_empty() {
        for line in summary_lines.into_iter().rev() {
            println!("    {}", style(line).dim());
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("short", 60), "short");
        assert_eq!(
            truncate("a".repeat(100).as_str(), 20),
            format!("{}...", "a".repeat(17))
        );
    }
}
