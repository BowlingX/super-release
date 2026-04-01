# E2E Comparison: super-release vs semantic-release

Verifies that super-release produces the same version bumps as semantic-release for identical git histories.

## Prerequisites

- Node.js / npx (for semantic-release)
- super-release binary (built from source)

## Usage

```bash
# Build super-release
cargo build --release

# Run comparison (default: 4 tests in parallel)
SUPER_RELEASE=./target/release/super-release bash e2e/compare.sh

# Run with more parallelism
PARALLEL=8 SUPER_RELEASE=./target/release/super-release bash e2e/compare.sh
```

## What it tests

Each scenario creates a fresh git repo, configures both tools with equivalent settings, and compares the computed next version.

- **Stable releases**: feat→minor, fix→patch, breaking→major, mixed commits, perf, chore (no release)
- **Prerelease branches**: first prerelease, increment, breaking on prerelease
- **Maintenance branches**: fix on 1.x, feat on 1.x, breaking capped to minor
- **Edge cases**: first release with no prior tags
