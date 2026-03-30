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

To migrate from semantic-release's tag format, set `tag_format_package: "{name}@{version}"`.

#### `plugins`

Ordered list of plugins to execute. Each plugin runs its `prepare` phase, then its `publish` phase. Each plugin accepts an `options` object for customization.

Default: `[changelog, npm, git-commit, git-tag]`

```yaml
plugins:
  - name: changelog
    options:
      filename: CHANGELOG.md      # output file per package (default: CHANGELOG.md)
      preview_lines: 20           # max lines shown in dry-run (default: 20)

  - name: npm
    options:
      access: public              # npm access level (default: "public")
      registry: https://registry.npmjs.org  # custom registry URL
      tag: next                   # dist-tag override (default: auto from prerelease channel)
      publish_args:               # extra args passed to the publish command
        - "--otp=123456"
      package_manager: yarn       # force specific PM (default: auto-detect)

  - name: git-commit
    options:
      # Commit message template. Placeholders:
      #   {releases} - comma-separated list: "@acme/core@1.1.0, @acme/utils@1.0.1"
      #   {summary}  - one per line: "  - @acme/core 1.0.0 -> 1.1.0"
      #   {count}    - number of packages released
      message: "chore(release): {releases} [skip ci]"
      push: false                 # push after commit (default: false)
      remote: origin              # git remote (default: "origin")
      paths:                      # paths to stage (default: ["."])
        - "."

  - name: git-tag
    options:
      push: false                 # push tags to remote after creation (default: false)
      remote: origin              # git remote to push to (default: "origin")
```

| Plugin | Prepare | Publish |
|---|---|---|
| `changelog` | Generates/updates changelog per package (parallel) | -- |
| `npm` | Updates `package.json` versions (auto-detects npm/yarn/pnpm) | Publishes packages (parallel within dependency levels) |
| `git-commit` | -- | Stages changed files, commits with release message, optionally pushes |
| `git-tag` | -- | Creates annotated git tags, optionally pushes |

The default plugin order ensures: changelogs and version bumps are written first, then committed, then tagged.

#### `packages`

Optional list of glob patterns to include. When set, only packages whose name matches at least one pattern are released. Supports `*`, `?`, `[...]`, and `{a,b}` alternation.

```yaml
# Only release packages in the @acme scope
packages:
  - "@acme/*"

# Release specific packages
packages:
  - "@acme/core"
  - "@acme/utils"

# Multiple scopes
packages:
  - "{@acme/*,@tools/*}"
```

Default: all discovered packages.

#### `exclude`

List of glob patterns to exclude from releasing. Applied after `packages`.

```yaml
exclude:
  - my-monorepo-root
  - "@acme/internal-*"
```

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
