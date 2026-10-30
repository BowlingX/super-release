use std::collections::HashMap;

use semver::Version;

use crate::commit::{ConventionalCommit, parse_conventional_commit};
use crate::package::Package;

/// Parse and attach the full oid, as production commits have (src/git.rs).
pub(crate) fn oc(sha40: &str, msg: &str) -> ConventionalCommit {
    let mut c = parse_conventional_commit(&sha40[..8], msg).unwrap();
    c.oid = Some(git2::Oid::from_str(sha40).unwrap());
    c
}

/// A workspace package with local dependencies; everything else defaulted.
pub(crate) fn make_pkg(name: &str, local_deps: &[&str]) -> Package {
    Package {
        name: name.to_string(),
        version: Version::new(1, 0, 0),
        path: format!("packages/{}", name).into(),
        manifest_path: format!("packages/{}/package.json", name).into(),
        is_root: false,
        local_dependencies: local_deps
            .iter()
            .map(|d| (d.to_string(), "^1.0.0".to_string()))
            .collect(),
        dependencies: HashMap::new(),
        dev_dependencies: HashMap::new(),
        optional_dependencies: HashMap::new(),
        warning: None,
        skipped: false,
    }
}
