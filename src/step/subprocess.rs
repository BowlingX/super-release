use console::style;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

/// Shared MultiProgress for coordinating concurrent spinners.
static MULTI: LazyLock<MultiProgress> = LazyLock::new(MultiProgress::new);

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

pub struct RunOptions<'a> {
    pub label: &'a str,
    pub step_name: &'a str,
}

/// Failed run of a command, carrying the captured output so callers can classify the failure before reporting it.
///
/// Deliberately not `std::error::Error`: `?` will not compile on it, so every caller must either classify
/// the failure or call [`CommandFailure::report`], which is the only place the output tail gets printed.
#[derive(Debug)]
pub struct CommandFailure {
    /// `[step] label`, used as the header of the printed output tail.
    context: String,
    message: String,
    pub output: Vec<String>,
}

impl CommandFailure {
    /// Prints the last 20 output lines on a TTY (CI already streamed every line) and converts into an error.
    /// The message itself is left to the returned error so callers do not print it twice.
    pub fn report(self) -> anyhow::Error {
        if !self.output.is_empty() && console::Term::stdout().is_term() {
            let tail = &self.output[self.output.len().saturating_sub(20)..];
            let _ = MULTI.println(format!(
                "  {}\n{}",
                style(format!("{} output:", self.context)).red(),
                tail.iter()
                    .map(|l| format!("    {}", style(l).dim()))
                    .collect::<Vec<_>>()
                    .join("\n")
            ));
        }
        anyhow::Error::msg(self.message)
    }
}

/// Run a command, streaming output live: a per-task spinner on TTY, prefixed lines in CI.
pub fn run_command(mut cmd: Command, opts: &RunOptions) -> Result<(), CommandFailure> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    let context = format!("[{}] {}", opts.step_name, opts.label);
    let failure = |message: String, output: Vec<String>| CommandFailure {
        context: context.clone(),
        message,
        output,
    };

    let mut child = cmd.spawn().map_err(|e| {
        failure(
            format!(
                "[{}] Failed to spawn command for {}: {}",
                opts.step_name, opts.label, e
            ),
            Vec::new(),
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

    let status = child.wait().map_err(|e| {
        failure(
            format!(
                "[{}] Failed to wait for {}: {}",
                opts.step_name, opts.label, e
            ),
            Vec::new(),
        )
    })?;
    let output = std::mem::take(&mut *all_output.lock().unwrap());

    if !status.success() {
        return Err(failure(
            format!(
                "[{}] Command failed for {} (exit code: {})",
                opts.step_name, opts.label, status
            ),
            output,
        ));
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
