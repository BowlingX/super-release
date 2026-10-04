//! Top-level output sections, rendered as collapsible log groups on CI platforms that support them.

use console::style;
use std::fmt::Display;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroupStyle {
    /// `::group::` workflow commands, also understood by Gitea and Forgejo Actions.
    GitHub,
    GitLab,
    Azure,
}

static IS_INTERACTIVE: LazyLock<bool> = LazyLock::new(|| console::Term::stdout().is_term());
static GROUP_STYLE: LazyLock<Option<GroupStyle>> =
    LazyLock::new(|| detect(|k| std::env::var(k).ok(), is_interactive()));

/// GitHub Actions and Azure Pipelines cannot nest groups, so at most one is open at a time.
static GROUP_OPEN: AtomicBool = AtomicBool::new(false);
static SECTION_ID: AtomicUsize = AtomicUsize::new(0);

/// Whether stdout is a terminal (spinners, truncated output) rather than a CI log (streamed lines, log groups).
pub fn is_interactive() -> bool {
    *IS_INTERACTIVE
}

fn detect(env: impl Fn(&str) -> Option<String>, is_tty: bool) -> Option<GroupStyle> {
    if is_tty {
        return None;
    }
    let is_true = |key: &str| env(key).is_some_and(|v| v.eq_ignore_ascii_case("true"));
    if is_true("GITHUB_ACTIONS") {
        Some(GroupStyle::GitHub)
    } else if is_true("GITLAB_CI") {
        Some(GroupStyle::GitLab)
    } else if is_true("TF_BUILD") {
        Some(GroupStyle::Azure)
    } else {
        None
    }
}

fn start_marker(kind: GroupStyle, title: &str, id: usize, now: i64) -> String {
    let title = console::strip_ansi_codes(title);
    match kind {
        GroupStyle::GitHub => format!("::group::{title}"),
        GroupStyle::GitLab => {
            format!("\x1b[0Ksection_start:{now}:super_release_{id}[collapsed=true]\r\x1b[0K{title}")
        }
        GroupStyle::Azure => format!("##[group]{title}"),
    }
}

fn end_marker(kind: GroupStyle, id: usize, now: i64) -> String {
    match kind {
        GroupStyle::GitHub => "::endgroup::".to_string(),
        GroupStyle::GitLab => format!("\x1b[0Ksection_end:{now}:super_release_{id}\r\x1b[0K"),
        GroupStyle::Azure => "##[endgroup]".to_string(),
    }
}

/// Guard for an output section; closes the CI log group when dropped, including on early `?` returns,
/// so a final error message is printed outside the collapsed group.
#[must_use = "the section ends when this guard is dropped"]
pub struct Section {
    /// Set when this section opened a log group.
    id: Option<usize>,
}

impl Drop for Section {
    fn drop(&mut self) {
        if let (Some(id), Some(kind)) = (self.id, *GROUP_STYLE) {
            printfl!("{}", end_marker(kind, id, chrono::Utc::now().timestamp()));
            GROUP_OPEN.store(false, Ordering::SeqCst);
        }
    }
}

/// Print a `>>` section header that always stays visible, outside any log group.
pub fn header(text: impl Display) {
    printfl!("{} {}", style(">>").bold().blue(), text);
}

/// Start a top-level section: a collapsible log group on supported CI platforms, a [`header`] line otherwise.
pub fn section(title: impl Display) -> Section {
    match *GROUP_STYLE {
        Some(kind) if !GROUP_OPEN.swap(true, Ordering::SeqCst) => {
            let id = SECTION_ID.fetch_add(1, Ordering::Relaxed);
            let title = title.to_string();
            printfl!(
                "{}",
                start_marker(kind, &title, id, chrono::Utc::now().timestamp())
            );
            Section { id: Some(id) }
        }
        _ => {
            header(title);
            Section { id: None }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(vars: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |k| {
            vars.iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
        }
    }

    #[test]
    fn test_detect_platforms() {
        assert_eq!(
            detect(env_of(&[("GITHUB_ACTIONS", "true")]), false),
            Some(GroupStyle::GitHub)
        );
        assert_eq!(
            detect(env_of(&[("GITLAB_CI", "true")]), false),
            Some(GroupStyle::GitLab)
        );
        assert_eq!(
            detect(env_of(&[("TF_BUILD", "True")]), false),
            Some(GroupStyle::Azure)
        );
        assert_eq!(detect(env_of(&[]), false), None);
        assert_eq!(detect(env_of(&[("CI", "true")]), false), None);
        assert_eq!(detect(env_of(&[("GITHUB_ACTIONS", "false")]), false), None);
    }

    #[test]
    fn test_detect_precedence() {
        let env = env_of(&[
            ("GITHUB_ACTIONS", "true"),
            ("GITLAB_CI", "true"),
            ("TF_BUILD", "True"),
        ]);
        assert_eq!(detect(env, false), Some(GroupStyle::GitHub));

        let env = env_of(&[("GITLAB_CI", "true"), ("TF_BUILD", "True")]);
        assert_eq!(detect(env, false), Some(GroupStyle::GitLab));
    }

    #[test]
    fn test_detect_disabled_on_tty() {
        assert_eq!(detect(env_of(&[("GITHUB_ACTIONS", "true")]), true), None);
    }

    #[test]
    fn test_markers() {
        let title = format!("Running step: {}", style("npm").bold().force_styling(true));

        assert_eq!(
            start_marker(GroupStyle::GitHub, &title, 0, 42),
            "::group::Running step: npm"
        );
        assert_eq!(end_marker(GroupStyle::GitHub, 0, 42), "::endgroup::");

        assert_eq!(
            start_marker(GroupStyle::Azure, &title, 0, 42),
            "##[group]Running step: npm"
        );
        assert_eq!(end_marker(GroupStyle::Azure, 0, 42), "##[endgroup]");

        assert_eq!(
            start_marker(GroupStyle::GitLab, &title, 0, 42),
            "\x1b[0Ksection_start:42:super_release_0[collapsed=true]\r\x1b[0KRunning step: npm"
        );
        assert_eq!(
            end_marker(GroupStyle::GitLab, 0, 43),
            "\x1b[0Ksection_end:43:super_release_0\r\x1b[0K"
        );
    }
}
