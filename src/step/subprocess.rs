use anyhow::{Context, Result};
use console::style;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

/// Shared MultiProgress for coordinating concurrent spinners.
static MULTI: LazyLock<MultiProgress> = LazyLock::new(MultiProgress::new);

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
    pub label: &'a str,
    pub step_name: &'a str,
}

/// Run a command with live output streaming:
/// - TTY: per-task spinner via MultiProgress (safe for concurrent use)
/// - CI (no TTY): all lines printed as they arrive with step prefix
/// - On error: last 20 lines for debugging (TTY only)
pub fn run_command(mut cmd: Command, opts: &RunOptions) -> Result<()> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let mut child = cmd.spawn().with_context(|| {
        format!(
            "[{}] Failed to spawn command for {}",
            opts.step_name, opts.label
        )
    })?;

    let is_tty = console::Term::stdout().is_term();
    let all_output: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));

    let spinner = if is_tty {
        let s = MULTI.add(ProgressBar::new_spinner());
        s.set_style(
            ProgressStyle::default_spinner()
                .template("  {spinner:.cyan} [{prefix}] {msg}")
                .unwrap(),
        );
        s.set_prefix(opts.step_name.to_string());
        s.set_message(format!("{}...", opts.label));
        s.enable_steady_tick(Duration::from_millis(80));
        Some(s)
    } else {
        None
    };

    let stdout = child.stdout.take();
    let output_clone = Arc::clone(&all_output);
    let spinner_clone = spinner.clone();
    let step_name = opts.step_name.to_string();
    let step_name_clone = step_name.clone();
    let is_tty_clone = is_tty;

    let stdout_handle = std::thread::spawn(move || {
        stream_lines(
            stdout,
            &output_clone,
            spinner_clone.as_ref(),
            is_tty_clone,
            &step_name_clone,
        );
    });
    stream_lines(
        child.stderr.take(),
        &all_output,
        spinner.as_ref(),
        is_tty,
        &step_name,
    );

    stdout_handle.join().ok();

    if let Some(ref s) = spinner {
        s.finish_and_clear();
    }

    let status = child.wait()?;
    let all_output = all_output.lock().unwrap();

    if !status.success() {
        if is_tty {
            let tail: Vec<&str> = all_output
                .iter()
                .map(|s| s.as_str())
                .rev()
                .take(20)
                .collect();
            let tail: Vec<&str> = tail.into_iter().rev().collect();
            MULTI.println(format!(
                "  [{}] {} output:\n{}",
                style(opts.step_name).red(),
                opts.label,
                tail.iter()
                    .map(|l| format!("    {}", style(l).dim()))
                    .collect::<Vec<_>>()
                    .join("\n")
            ))?;
        }

        anyhow::bail!(
            "[{}] Command failed for {} (exit code: {})",
            opts.step_name,
            opts.label,
            status
        );
    }

    Ok(())
}

fn stream_lines<R: std::io::Read>(
    reader: Option<R>,
    output: &Arc<Mutex<Vec<String>>>,
    spinner: Option<&ProgressBar>,
    is_tty: bool,
    step_name: &str,
) {
    let Some(r) = reader else { return };
    for line in BufReader::new(r).lines() {
        let line = line.unwrap_or_default();

        if is_tty {
            if let Some(s) = spinner {
                s.set_message(truncate(&line, 60));
            }
        } else {
            println!("    [{}] {}", style(step_name).dim(), line);
        }

        output.lock().unwrap().push(line);
    }
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
