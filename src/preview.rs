//! Renders a Markdown "release preview" for a pull request: the next version
//! per package and a collapsible preview of each package's release notes.

use std::collections::HashSet;
use std::fmt::Write;

use crate::config::Config;
use crate::notes::generate_release_notes;
use crate::version::PackageRelease;

/// Stable HTML marker used to find and update the sticky preview comment.
pub const PREVIEW_MARKER: &str = "<!-- super-release:preview -->";

/// Render the release preview as Markdown; the first line is always [`PREVIEW_MARKER`] so the comment can be updated in place, and notes appear only for packages in `notes_packages`.
pub fn render_preview_markdown(
    releases: &[PackageRelease],
    notes_packages: &HashSet<String>,
    changelog_template: Option<&str>,
    cfg: &Config,
) -> String {
    let mut out = String::new();
    out.push_str(PREVIEW_MARKER);
    out.push('\n');
    out.push_str("## 📦 Release preview\n\n");

    if releases.is_empty() {
        out.push_str("No release will be triggered by this pull request.\n");
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
            Some(reason) => format!("{} (via {})", r.bump, reason),
            None => r.bump.to_string(),
        };
        let _ = writeln!(
            out,
            "| `{}` | {} | `{}` → `{}` | `{}` |",
            r.package_name, bump, r.current_version, r.next_version, tag
        );
    }
    out.push('\n');

    for r in releases {
        if !notes_packages.contains(&r.package_name) {
            continue;
        }
        let notes = generate_release_notes(r, changelog_template)
            .unwrap_or_else(|e| format!("_Failed to render release notes: {}_", e));
        let _ = write!(
            out,
            "<details>\n<summary><code>{}@{}</code> release notes</summary>\n\n{}\n\n</details>\n\n",
            r.package_name,
            r.next_version,
            notes.trim()
        );
    }

    out.push_str(
        "> ℹ️ Preview based on the current commits in this branch — the final \
         release may differ (e.g. after a squash-merge).\n",
    );
    out
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
        let md = render_preview_markdown(&[], &HashSet::new(), None, &Config::default());
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
        let md = render_preview_markdown(&releases, &notes, None, &Config::default());

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
        let md = render_preview_markdown(&releases, &notes, None, &Config::default());

        assert!(md.contains("| `root-pkg` |"));
        assert!(md.contains("| `other-pkg` |"));
        assert_eq!(md.matches("<details>").count(), 1);
        assert!(md.contains("<code>root-pkg@1.1.0</code>"));
        assert!(!md.contains("<code>other-pkg@2.3.2</code>"));
    }
}
