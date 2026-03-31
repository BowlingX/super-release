pub mod node;

use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::package::Package;
use crate::version::PackageRelease;

/// Trait for discovering and resolving packages in a repository.
///
/// Each implementation handles a specific ecosystem (Node.js, Cargo, Python, etc.).
pub trait PackageResolver {
    /// Discover all packages in the repository.
    fn discover(&self, repo_root: &Path) -> Result<Vec<Package>>;

    /// Resolve local (in-repo) dependencies between discovered packages.
    fn resolve_dependencies(&self, packages: &mut [Package]);

    /// Update package manifest versions on disk. Returns the list of modified files.
    fn bump_versions(
        &self,
        repo_root: &Path,
        packages: &[Package],
        releases: &[PackageRelease],
        dry_run: bool,
    ) -> Result<Vec<PathBuf>>;
}

/// Create a resolver by name.
pub fn create_resolver(name: &str) -> Option<Box<dyn PackageResolver>> {
    match name {
        "node" => Some(Box::new(node::NodeResolver)),
        _ => None,
    }
}
