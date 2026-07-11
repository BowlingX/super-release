//! Renders a Markdown "release preview" for a pull request: the next version
//! per package and a collapsible preview of each package's release notes.

use crate::config::Config;
use crate::step::changelog::generate_release_notes;
use crate::version::PackageRelease;

/// Stable HTML marker used to find and update the sticky preview comment.
pub const PREVIEW_MARKER: &str = "<!-- super-release:preview -->";

/// Render the release preview as GitHub-flavored Markdown. The first line is
/// always [`PREVIEW_MARKER`] so the comment can be located and updated in place.
pub fn render_preview_markdown(releases: &[PackageRelease], cfg: &Config) -> String {
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
        out.push_str(&format!(
            "| `{}` | {} | `{}` → `{}` | `{}` |\n",
            r.package_name, bump, r.current_version, r.next_version, tag
        ));
    }
    out.push('\n');

    for r in releases {
        let notes = generate_release_notes(r)
            .unwrap_or_else(|e| format!("_Failed to render release notes: {}_", e));
        out.push_str(&format!(
            "<details>\n<summary><code>{}@{}</code> release notes</summary>\n\n{}\n\n</details>\n\n",
            r.package_name,
            r.next_version,
            notes.trim()
        ));
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
        let md = render_preview_markdown(&[], &Config::default());
        assert!(md.starts_with(PREVIEW_MARKER));
        assert!(md.contains("No release will be triggered"));
    }

    #[test]
    fn renders_marker_table_and_details_per_release() {
        let releases = vec![
            release("root-pkg", (1, 0, 0), (1, 1, 0)),
            release("other-pkg", (2, 3, 1), (2, 3, 2)),
        ];
        let md = render_preview_markdown(&releases, &Config::default());

        assert!(md.starts_with(PREVIEW_MARKER));
        assert!(md.contains("| `root-pkg` |"));
        assert!(md.contains("| `other-pkg` |"));
        assert!(md.contains("`1.0.0` → `1.1.0`"));
        // One collapsible block per release.
        assert_eq!(md.matches("<details>").count(), 2);
    }
}
