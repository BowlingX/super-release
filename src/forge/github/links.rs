//! GitHub URL construction, kept apart from the neutral step layer because path shapes are provider-specific.

use crate::forge::RepoRef;

/// The tag is a single path segment, so a `/` in it (monorepo tags like `pkg/v1.2.3`) must be percent-encoded.
pub(crate) fn release_url(repo: &RepoRef, tag: &str) -> String {
    format!("{}/releases/tag/{}", repo.web_url(), encode_tag(tag))
}

/// Percent-encode a tag as a single URL path segment, matching GitHub's own encoding (only path-breaking characters are escaped).
fn encode_tag(tag: &str) -> String {
    use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
    const TAG: &AsciiSet = &CONTROLS
        .add(b' ')
        .add(b'"')
        .add(b'#')
        .add(b'%')
        .add(b'/')
        .add(b'<')
        .add(b'>')
        .add(b'?')
        .add(b'`')
        .add(b'{')
        .add(b'}');
    utf8_percent_encode(tag, TAG).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_url_encodes_tag_path_segment() {
        let r = RepoRef {
            owner: "o".into(),
            repo: "r".into(),
            host: "github.com".into(),
        };
        assert_eq!(
            release_url(&r, "super-release/v1.10.0"),
            "https://github.com/o/r/releases/tag/super-release%2Fv1.10.0"
        );
        assert_eq!(
            release_url(&r, "v1.10.0"),
            "https://github.com/o/r/releases/tag/v1.10.0"
        );
    }
}
