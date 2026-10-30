//! Renders a Markdown "release preview" for a pull request: the next version
//! per package and a collapsible preview of each package's release notes.

use std::collections::HashSet;
use std::fmt::Write;

use crate::config::Config;
use crate::notes::generate_release_notes;
use crate::pr_notes::{self, IgnoreReason, ReleaseMetadata};
use crate::version::PackageRelease;

/// Stable HTML marker used to find and update the sticky preview comment.
pub const PREVIEW_MARKER: &str = "<!-- super-release:preview -->";

/// Render the release preview as Markdown; the first line is always [`PREVIEW_MARKER`] so the comment can be updated in place, and notes appear only for packages in `notes_packages`. `meta` adds the PR-comment notes.
pub fn render_preview_markdown(
    releases: &[PackageRelease],
    notes_packages: &HashSet<String>,
    changelog_template: Option<&str>,
    cfg: &Config,
    meta: Option<&ReleaseMetadata>,
) -> String {
    let mut out = String::new();
    out.push_str(PREVIEW_MARKER);
    out.push('\n');
    out.push_str("## 📦 Release preview\n\n");

    if releases.is_empty() {
        out.push_str("No release will be triggered by this pull request.\n");
        if let Some(meta) = meta {
            render_ignored_directives(&mut out, meta);
        }
        return out;
    }

    out.push_str(
        "The following release(s) would be published when this pull request is merged:\n\n",
    );
    out.push_str("| Package | Bump | Version | Tag |\n");
    out.push_str("| --- | --- | --- | --- |\n");
    for r in releases {
        let tag = cfg.format_tag(&r.package_name, &r.next_version, r.is_root);
        let bump = match &r.propagated_from {
            Some(chain) => format!("{} (via {})", r.bump, chain.join(" -> ")),
            None => r.bump.to_string(),
        };
        let _ = writeln!(
            out,
            "| `{}` | {} | `{}` → `{}` | `{}` |",
            r.package_name, bump, r.current_version, r.next_version, tag
        );
    }
    out.push('\n');

    let notes_for = |r: &PackageRelease| meta.and_then(|m| m.notes_for(&r.package_name));

    for r in releases {
        if !notes_packages.contains(&r.package_name) {
            continue;
        }
        let notes = generate_release_notes(r, changelog_template, notes_for(r))
            .unwrap_or_else(|e| format!("_Failed to render release notes: {}_", e));
        let _ = write!(
            out,
            "<details>\n<summary><code>{}@{}</code> release notes</summary>\n\n{}\n\n</details>\n\n",
            r.package_name,
            r.next_version,
            notes.trim()
        );
    }

    if let Some(meta) = meta {
        render_note_sources(&mut out, releases, meta);
        render_ignored_directives(&mut out, meta);
    }

    out.push_str(
        "> ℹ️ Preview based on the current commits in this branch — the final \
         release may differ (e.g. after a squash-merge).\n",
    );
    if cfg.release_notes.pr_comments {
        out.push_str(
            ">\n> ✍️ To write the release notes yourself, comment `@super-release release-notes` \
             (or `@super-release release-notes@<package>`) followed by Markdown. It replaces the \
             notes generated from this PR's commits; the newest comment wins and an empty one \
             restores the generated notes.\n",
        );
    }
    out
}

/// Which PR comments supply each package's notes.
fn render_note_sources(out: &mut String, releases: &[PackageRelease], meta: &ReleaseMetadata) {
    let lines: Vec<String> = releases
        .iter()
        .filter_map(|r| {
            let notes = meta
                .notes_for(&r.package_name)
                .filter(|n| !n.blocks.is_empty())?;
            let sources: Vec<String> = notes
                .blocks
                .iter()
                .map(|b| {
                    let by = b
                        .author
                        .as_deref()
                        .map(|a| format!(" by @{a}"))
                        .unwrap_or_default();
                    format!("#{} ([comment]({}){by})", b.pr_number, b.comment_url)
                })
                .collect();
            Some(format!("- `{}`: {}\n", r.package_name, sources.join(", ")))
        })
        .collect();
    if !lines.is_empty() {
        out.push_str("**✍️ Release notes from PR comments**\n\n");
        out.extend(lines);
        out.push('\n');
    }
}

/// Directive comments that don't count, and why.
fn render_ignored_directives(out: &mut String, meta: &ReleaseMetadata) {
    let mut any = false;
    for found in pr_notes::directive_comments(meta) {
        let Some(reason) = found.ignored else {
            continue;
        };
        let author = found.comment.author.as_deref().unwrap_or("ghost");
        let why = match reason {
            IgnoreReason::Untrusted => format!(
                "@{author}'s association (`{}`) is not in `release_notes.trusted_authors`",
                found.comment.association
            ),
            IgnoreReason::UnknownPackage(target) => {
                format!("no released package with release notes matches `{target}`")
            }
        };
        let _ = writeln!(
            out,
            "> ⚠️ Ignored [release notes comment]({}) on #{}: {}.",
            found.comment.url, found.pr, why
        );
        any = true;
    }
    if any {
        out.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commit::BumpLevel;
    use semver::Version;

    fn release(name: &str, cur: (u64, u64, u64), next: (u64, u64, u64)) -> PackageRelease {
        PackageRelease {
            package_name: name.into(),
            current_version: Version::new(cur.0, cur.1, cur.2),
            next_version: Version::new(next.0, next.1, next.2),
            bump: BumpLevel::Minor,
            commits: vec![],
            is_root: true,
            propagated_from: None,
        }
    }

    #[test]
    fn empty_releases_render_no_release_message() {
        let md = render_preview_markdown(&[], &HashSet::new(), None, &Config::default(), None);
        assert!(md.starts_with(PREVIEW_MARKER));
        assert!(md.contains("No release will be triggered"));
    }

    #[test]
    fn renders_marker_table_and_details_per_release() {
        let releases = vec![
            release("root-pkg", (1, 0, 0), (1, 1, 0)),
            release("other-pkg", (2, 3, 1), (2, 3, 2)),
        ];
        let notes: HashSet<String> = ["root-pkg", "other-pkg"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let md = render_preview_markdown(&releases, &notes, None, &Config::default(), None);

        assert!(md.starts_with(PREVIEW_MARKER));
        assert!(md.contains("| `root-pkg` |"));
        assert!(md.contains("| `other-pkg` |"));
        assert!(md.contains("`1.0.0` → `1.1.0`"));
        assert_eq!(md.matches("<details>").count(), 2);
    }

    #[test]
    fn notes_shown_only_for_changelog_covered_packages() {
        let releases = vec![
            release("root-pkg", (1, 0, 0), (1, 1, 0)),
            release("other-pkg", (2, 3, 1), (2, 3, 2)),
        ];
        let notes: HashSet<String> = std::iter::once("root-pkg".to_string()).collect();
        let md = render_preview_markdown(&releases, &notes, None, &Config::default(), None);

        assert!(md.contains("| `root-pkg` |"));
        assert!(md.contains("| `other-pkg` |"));
        assert_eq!(md.matches("<details>").count(), 1);
        assert!(md.contains("<code>root-pkg@1.1.0</code>"));
        assert!(!md.contains("<code>other-pkg@2.3.2</code>"));
    }
}
