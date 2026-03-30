# super-release

A fast [semantic-release](https://semantic-release.gitbook.io/semantic-release) alternative for monorepos, written in Rust.

Analyzes [conventional commits](https://www.conventionalcommits.org/) to determine version bumps, generate changelogs, update `package.json` files, publish to npm, and create git tags -- across all packages in a monorepo, in parallel.

## Features

- Monorepo-first: discovers all `package.json` packages and associates commits by changed files
- Parallel commit analysis using rayon (configurable with `-j`)
- Prerelease branches (`beta`, `next`, or dynamic from branch name)
- Maintenance branches (`1.x`, `2.x`) with major-version capping
- Changelog generation powered by [git-cliff](https://git-cliff.org/)
- Plugin system: changelog, npm, git-tag (extensible)
- Configurable tag format templates
- Dependency-aware npm publish (topological order)
- Dry-run mode with pretty, truncated output
- Legacy tag format migration (`@scope/pkg@1.0.0`)

## Installation

```bash
cargo install --path .
```

Or build a release binary:

```bash
cargo build --release
# Binary at target/release/super-release
```

## Quick Start

```bash
# Preview what would be released
super-release --dry-run

# Run a release
super-release

# Only release specific packages
super-release -p core -p utils

# Use 4 threads for commit analysis
super-release -j 4
```

## CLI Reference

```
Usage: super-release [OPTIONS]

Options:
  -n, --dry-run            Show what would happen without making changes
  -C, --path <PATH>        Repository root [default: .]
  -c, --config <CONFIG>    Path to config file [default: .release.yaml]
  -p, --package <PACKAGE>  Only process specific packages (repeatable)
  -v, --verbose            Verbose output
  -j, --jobs <JOBS>        Parallel jobs for commit analysis [default: 50% of CPUs]
  -h, --help               Print help
  -V, --version            Print version
```

## How It Works

1. **Discover packages** -- finds all directories with a `package.json`
2. **Resolve tags** -- finds the latest release tag per package (filtered by branch context)
3. **Walk commits** -- only analyzes commits since the oldest tag (not the entire history)
4. **Associate commits to packages** -- maps changed files to their owning package
5. **Calculate versions** -- uses git-cliff's conventional commit analysis to determine bump levels
6. **Run plugins** -- changelog, npm publish, git tag (in configured order)

## Conventional Commits

super-release follows the [Conventional Commits](https://www.conventionalcommits.org/) specification:

| Commit | Bump |
|---|---|
| `fix: ...` | patch |
| `feat: ...` | minor |
| `feat!: ...` or `BREAKING CHANGE:` in footer | major |
| `perf: ...` | patch |
| `chore: ...`, `docs: ...`, `ci: ...` | no release |

## Configuration

Create a `.release.yaml` (or `.release.yml`, `.super-release.yaml`) in your repository root. All fields are optional and have sensible defaults.

### Full Example

```yaml
# Branch configurations
branches:
  # Stable branches (simple string = stable, no prerelease)
  - main
  - master

  # Prerelease with a fixed channel name
  - name: beta
    prerelease: beta          # -> 2.0.0-beta.1, 2.0.0-beta.2, ...

  - name: next
    prerelease: next          # -> 2.0.0-next.1, ...

  # Prerelease using the branch name as the channel (for branch patterns)
  - name: "test-*"
    prerelease: true          # branch test-foo -> 2.0.0-test-foo.1, ...

  # Maintenance branches (caps major version, breaking changes -> minor)
  - name: "1.x"
    maintenance: true         # -> 1.5.1, 1.6.0 (never 2.0.0)

# Tag format templates (use {version} and {name} placeholders)
tag_format: "v{version}"                  # root package: v1.2.3
tag_format_package: "{name}/v{version}"   # sub-packages: @acme/core/v1.2.3

# Packages to exclude from releasing (substring match on package name)
exclude:
  - my-private-pkg

# Plugins run in order: prepare phase first, then publish phase
plugins:
  - name: changelog
  - name: npm
  - name: git-tag
```

### Reference

#### `branches`

Defines which branches can produce releases and what kind.

| Form | Type | Example versions |
|---|---|---|
| `- main` | Stable | `1.0.0`, `1.1.0`, `2.0.0` |
| `- name: beta`<br>`  prerelease: beta` | Prerelease (fixed channel) | `2.0.0-beta.1`, `2.0.0-beta.2` |
| `- name: "test-*"`<br>`  prerelease: true` | Prerelease (branch name as channel) | `2.0.0-test-my-feature.1` |
| `- name: "1.x"`<br>`  maintenance: true` | Maintenance | `1.5.1`, `1.6.0` (major capped) |

**Tag filtering by branch**: Stable branches only see stable tags. Prerelease branches see their own channel's tags plus stable tags. This prevents a `v2.0.0-beta.1` tag from affecting version calculation on `main`.

**Prerelease behavior**: If the latest tag for a package is already on the same prerelease channel (e.g. `v2.0.0-beta.3`), the next release increments the prerelease number (`v2.0.0-beta.4`). If coming from a stable version, it computes the next stable bump and appends the channel (`v1.1.0-beta.1`).

**Maintenance behavior**: Breaking changes (`feat!:`) are demoted to minor bumps so the major version never increases on a maintenance branch.

Default: `["main", "master"]`

#### `tag_format`

Template for root package tags. Placeholders:
- `{version}` -- the semver version (e.g. `1.2.3`, `2.0.0-beta.1`)
- `{name}` -- the package name from `package.json`

Default: `"v{version}"`

Examples:
```yaml
tag_format: "v{version}"              # -> v1.2.3
tag_format: "release-{version}"       # -> release-1.2.3
tag_format: "{name}-v{version}"       # -> my-app-v1.2.3
```

#### `tag_format_package`

Template for sub-package tags in a monorepo.

Default: `"{name}/v{version}"`

Examples:
```yaml
tag_format_package: "{name}/v{version}"    # -> @acme/core/v1.2.3
tag_format_package: "{name}@{version}"     # -> @acme/core@1.2.3 (semantic-release compat)
```

**Legacy support**: Tags in `{name}@{version}` format are always recognized as a fallback, regardless of the configured template. This allows migrating from semantic-release without retagging.

#### `plugins`

Ordered list of plugins to execute. Each plugin runs its `prepare` phase (all plugins), then its `publish` phase (all plugins).

| Plugin | Prepare | Publish |
|---|---|---|
| `changelog` | Generates/updates `CHANGELOG.md` per package | -- |
| `npm` | Updates `package.json` versions + interdependencies | Runs `npm publish` in topological order |
| `git-tag` | -- | Creates annotated git tags |

Default: `[changelog, npm, git-tag]`

#### `exclude`

List of package name substrings to exclude from releasing. Useful for private root packages in monorepos.

```yaml
exclude:
  - my-monorepo-root
  - internal-tools
```

#### `packages`

Optional list of package name patterns to include. When set, only matching packages are released. By default, all discovered packages are included.

## Monorepo Structure

super-release discovers packages by finding `package.json` files recursively (skipping `node_modules`, `.git`, `dist`, `build`). Each commit is associated to a package based on which files it changed.

```
my-monorepo/
  package.json              <- root package (tags: v1.0.0)
  .release.yaml
  packages/
    core/
      package.json          <- @acme/core (tags: @acme/core/v1.0.0)
      src/
    utils/
      package.json          <- @acme/utils (tags: @acme/utils/v1.0.0)
      src/
```

**Dependency-aware publishing**: The npm plugin builds a dependency graph from `dependencies`, `devDependencies`, and `peerDependencies`. Packages are published in topological order (dependencies before dependents), and interdependency version ranges are updated automatically (preserving `^`/`~` prefixes).

## Performance

super-release is designed to be fast:

- **Parallel diff computation**: commit diffs are computed across multiple threads (thread-local git repo handles)
- **Tag-bounded history walk**: only walks commits since the oldest package tag, not the entire history
- **Single-pass commit collection**: commits are fetched once and partitioned per package
- **Precomputed file mapping**: file-to-package association is computed once, not per-package

Benchmark (2001 commits, 8 packages, Apple Silicon):
| Scenario | Time |
|---|---|
| All commits since initial tag | 0.11s |
| 100 commits since recent tags | 0.035s |

## License

MIT
