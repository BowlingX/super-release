//! Rendering of the "released" success comments posted on the PRs/issues a
//! release resolves.

use std::collections::BTreeMap;

use super::{GithubOptions, render_release_name};
use crate::commit::referenced_issues;
use crate::forge::{self, RepoRef};
use crate::step::ReleaseContext;
use crate::version::PackageRelease;

/// A release a resolved issue/PR shipped in: its display name (`release_name_template`,
/// e.g. `v1.10.0`) for the comment's link text and its tag for the release URL.
struct ReleasedIn {
    name: String,
    tag: String,
}

/// Aggregate the issues/PRs each release resolves into one comment per number, mentioning every release that included it (a PR may touch several packages).
pub(super) fn build_success_comments(
    ctx: &ReleaseContext,
    opts: &GithubOptions,
    releases: &[PackageRelease],
    repo: Option<&RepoRef>,
) -> Vec<forge::IssueComment> {
    let mut id_to_releases: BTreeMap<String, Vec<ReleasedIn>> = BTreeMap::new();
    for release in releases {
        let tag = ctx.cfg.format_tag(
            &release.package_name,
            &release.next_version,
            release.is_root,
        );
        let name = render_release_name(
            opts.release_name_template.as_deref(),
            &release.package_name,
            &release.next_version.to_string(),
            &tag,
        );
        for commit in &release.commits {
            for id in referenced_issues(&commit.raw_message) {
                let entry = id_to_releases.entry(id).or_default();
                if !entry.iter().any(|r| r.tag == tag) {
                    entry.push(ReleasedIn {
                        name: name.clone(),
                        tag: tag.clone(),
                    });
                }
            }
        }
    }

    id_to_releases
        .into_iter()
        .map(|(id, released_in)| forge::IssueComment {
            id,
            body: render_success_comment(opts.success_comment.as_deref(), &released_in, repo),
            labels: opts.released_labels.clone(),
        })
        .collect()
}

fn render_success_comment(
    template: Option<&str>,
    released_in: &[ReleasedIn],
    repo: Option<&RepoRef>,
) -> String {
    let releases = released_in
        .iter()
        .map(|r| match repo {
            Some(repo) => format!("[{}]({})", r.name, forge::github::release_url(repo, &r.tag)),
            None => format!("`{}`", r.name),
        })
        .collect::<Vec<_>>()
        .join(", ");
    match template {
        Some(t) => t
            .replace("{releases}", &releases)
            .replace("{tag}", released_in.first().map_or("", |r| r.tag.as_str())),
        None => format!(
            "🎉 This is included in the following release(s): {}",
            releases
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn released_in(name: &str, tag: &str) -> ReleasedIn {
        ReleasedIn {
            name: name.into(),
            tag: tag.into(),
        }
    }

    fn test_repo() -> RepoRef {
        RepoRef {
            owner: "o".into(),
            repo: "r".into(),
            host: "github.com".into(),
        }
    }

    #[test]
    fn success_comment_default_links_releases_by_name() {
        let releases = vec![
            released_in("v1.1.0", "super-release/v1.1.0"),
            released_in("core-v2.0.0", "core/v2.0.0"),
        ];
        let body = render_success_comment(None, &releases, Some(&test_repo()));
        assert!(
            body.contains("[v1.1.0](https://github.com/o/r/releases/tag/super-release%2Fv1.1.0)")
        );
        assert!(body.contains("[core-v2.0.0](https://github.com/o/r/releases/tag/core%2Fv2.0.0)"));
    }

    #[test]
    fn success_comment_without_repo_lists_names() {
        // Dry run: no published release to link to, so fall back to plain names.
        let releases = vec![released_in("v1.1.0", "super-release/v1.1.0")];
        let body = render_success_comment(None, &releases, None);
        assert!(body.contains("`v1.1.0`"));
        assert!(!body.contains("http"));
    }

    #[test]
    fn success_comment_template_substitutes() {
        let releases = vec![released_in("v1.1.0", "super-release/v1.1.0")];
        let body = render_success_comment(
            Some("Shipped in {tag} ({releases})"),
            &releases,
            Some(&test_repo()),
        );
        assert_eq!(
            body,
            "Shipped in super-release/v1.1.0 \
             ([v1.1.0](https://github.com/o/r/releases/tag/super-release%2Fv1.1.0))"
        );
    }
}
