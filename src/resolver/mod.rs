pub mod node;

use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::package::Package;
use crate::version::PackageRelease;

/// Trait for discovering and resolving packages in a repository.
pub trait PackageResolver {
    fn discover(&self, repo_root: &Path) -> Result<Vec<Package>>;

    /// Resolve local (in-repo) dependencies between discovered packages.
    fn resolve_dependencies(&self, packages: &mut [Package]);

    fn bump_versions(
        &self,
        repo_root: &Path,
        packages: &[Package],
        releases: &[PackageRelease],
        dry_run: bool,
    ) -> Result<Vec<PathBuf>>;
}

pub fn create_resolver(name: &str) -> Option<Box<dyn PackageResolver>> {
    match name {
        "node" => Some(Box::new(node::NodeResolver)),
        _ => None,
    }
}
