use clap::Parser;
use std::path::PathBuf;

/// Version: prefer SUPER_RELEASE_VERSION env at runtime, fallback to Cargo.toml.
fn runtime_version() -> &'static str {
    match std::env::var("SUPER_RELEASE_VERSION") {
        Ok(v) => Box::leak(v.into_boxed_str()),
        Err(_) => env!("CARGO_PKG_VERSION"),
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "super-release",
    about = "A fast semantic-release alternative for monorepos",
    version = runtime_version(),
    author
)]
pub struct Cli {
    /// Run in dry-run mode (show what would happen without making changes)
    #[arg(long, short = 'n')]
    pub dry_run: bool,

    /// Path to the repository root (defaults to current directory)
    #[arg(long, short = 'C', default_value = ".")]
    pub path: PathBuf,

    /// Path to the config file (defaults to .release.yaml in repo root)
    #[arg(long, short = 'c')]
    pub config: Option<PathBuf>,

    /// Print the next version for a package and exit.
    /// Use --package to select which package (defaults to root).
    #[arg(long)]
    pub show_next_version: bool,

    /// Render a release preview (next versions + notes) for a pull request and
    /// exit. Posts/updates a sticky PR comment when a GitHub token and PR are
    /// detected; otherwise prints the Markdown to stdout. Makes no changes.
    #[arg(long)]
    pub preview: bool,

    /// Pull request number/id for --preview (defaults to the PR detected from
    /// the CI environment).
    #[arg(long)]
    pub pr: Option<String>,

    /// GitHub repository as `owner/name` for --preview (defaults to the git
    /// remote or the GITHUB_REPOSITORY environment variable).
    #[arg(long)]
    pub repo: Option<String>,

    /// Base branch to evaluate --preview against (defaults to the PR base
    /// branch, GITHUB_BASE_REF, or the first configured release branch).
    #[arg(long)]
    pub base: Option<String>,

    /// With --preview, never post a PR comment; always print Markdown to stdout.
    #[arg(long)]
    pub no_comment: bool,

    /// Filter to a specific package (used with --show-next-version)
    #[arg(long, short = 'p')]
    pub package: Option<String>,

    /// Verbose output
    #[arg(long, short = 'v')]
    pub verbose: bool,

    /// Skip config file validation against the JSON schema
    #[arg(long)]
    pub dangerously_skip_config_check: bool,
}
