# super-release

A fast and opinionated [semantic-release](https://semantic-release.gitbook.io/semantic-release) alternative for
monorepos, written in Rust.

Analyzes [conventional commits](https://www.conventionalcommits.org/) to determine version bumps, generate changelogs,
update `package.json` files, publish to npm, and create git tags -- across all packages in a monorepo, in parallel.

## Features

- Monorepo-first: discovers all `package.json` packages and associates commits by changed files
- Prerelease branches (`beta`, `next`, or dynamic from branch name)
- Maintenance branches (`1.x`, `2.x`) with major-version capping
- Changelog generation powered by [git-cliff](https://git-cliff.org/)
- Auto-detects package manager (npm, yarn, pnpm)
- Configurable tag format templates
- Build in steps: changelog, npm, exec
- Global file dependencies and ignore patterns
- Idempotent: safe to rerun after partial failures
- Dry-run mode with pretty, truncated output

## Installation

The easiest way -- no install needed:

```bash
npx -y super-release --dry-run
```

Or install as a dev dependency:

```bash
pnpm add -D super-release
```

The npm package automatically downloads the prebuilt native binary for your platform on first run.

Supported platforms: Linux (x86_64, aarch64, musl/Alpine), macOS (x86_64, Apple Silicon), Windows (x86_64).

Alternatively, build from source:

```bash
cargo install --path .
```

## Quick Start

```bash
# Preview what would be released
super-release --dry-run

# Run a release
super-release

# Get the next version for a package (useful in CI scripts)
super-release --show-next-version

# Get the next version for a specific package in a monorepo
super-release --show-next-version --package @acme/core

```

## CLI Reference

```
Usage: super-release [OPTIONS]

Options:
  -n, --dry-run                Show what would happen without making changes
  -C, --path <PATH>            Repository root [default: .]
  -c, --config <CONFIG>        Path to config file [default: .release.yaml]
      --show-next-version      Print the next version and exit
  -p, --package <PACKAGE>      Filter to a specific package (for --show-next-version)
  -v, --verbose                Verbose output
      --dangerously-skip-config-check
                               Skip config file validation against the JSON schema
  -h, --help                   Print help
  -V, --version                Print version
```

### `--show-next-version`

Outputs only the next version (or the current version if no bump is needed) and exits silently. Useful for CI scripts:

```bash
VERSION=$(super-release --show-next-version)
SUPER_RELEASE_VERSION=$VERSION cargo build --release
```

In monorepos, use `--package` to select which package: `super-release --show-next-version -p @acme/core`

## How It Works

1. **Discover packages** -- finds all directories with a `package.json` (respects `.gitignore`)
2. **Resolve tags** -- finds the latest release tag per package (filtered by branch context, only reachable from HEAD)
3. **Walk commits** -- only analyzes commits since the oldest tag (not the entire history)
4. **Associate commits to packages** -- maps changed files to their owning package (respects `dependencies` and `ignore`
   config)
5. **Calculate versions** -- determines bump levels from conventional commits
6. **Run steps** -- changelog, npm publish, exec commands
7. **Git finalize** -- commits modified files, creates tags, optionally pushes

## Conventional Commits

| Commit                                                | Bump       |
|-------------------------------------------------------|------------|
| `fix: ...`                                            | patch      |
| `feat: ...`                                           | minor      |
| `feat!: ...` or `BREAKING CHANGE:` in footer          | major      |
| `perf: ...`                                           | patch      |
| `chore: ...`, `docs: ...`, `ci: ...`, `refactor: ...` | no release |

## Configuration

Create a `.release.yaml` in your repository root. JSON (`.release.json`) and JSONC (`.release.jsonc`) are also
supported. All fields are optional with sensible defaults.

The config is validated against a bundled [JSON Schema](schema.json) at startup. Use `--dangerously-skip-config-check`
to bypass validation.

For editor autocompletion:

```yaml
# yaml-language-server: $schema=https://raw.githubusercontent.com/bowlingx/super-release/main/schema.json
```

```jsonc
// .release.jsonc
{
  "$schema": "https://raw.githubusercontent.com/bowlingx/super-release/main/schema.json"
}
```

### Full Example

```yaml
branches:
  - main
  - name: next
    channel: next                  # publishes to "next" npm dist-tag
  - name: next-major
    channel: next-major
  - name: beta
    prerelease: beta
  - name: "test-*"
    prerelease: true
    packages: [ "@acme/core" ]    # only release core on test branches
  - name: "1.x"
    maintenance: true

tag_format: "v{version}"
tag_format_package: "{name}/v{version}"

packages:
  - "@acme/*"

exclude:
  - my-monorepo-root

# Files that trigger ALL packages when changed
dependencies:
  - yarn.lock
  - pnpm-lock.yaml

# Files to ignore -- commits touching only these won't trigger releases
ignore:
  - "README.md"
  - "docs/**"
  - "**/*.md"

steps:
  - name: changelog
  - name: npm
    options:
      provenance: true
  - name: exec
    options:
      prepare_cmd: "sed -i'' -e 's/^version = .*/version = \"{version}\"/' Cargo.toml"
      files:
        - Cargo.toml
        - Cargo.lock

git:
  commit_message: "chore(release): {releases} [skip ci]"
  push: false
  remote: origin
```

### Reference

#### `branches`

Defines which branches can produce releases. Only configured branches are allowed -- running on an unconfigured branch
exits cleanly.

| Form                                            | Type                                | Example versions                 |
|-------------------------------------------------|-------------------------------------|----------------------------------|
| `- main`                                        | Stable (primary)                    | `1.0.0`, `1.1.0`, `2.0.0`        |
| `- name: next`<br>`  channel: next`             | Stable (next channel)               | `1.1.0` on `next` dist-tag       |
| `- name: next-major`<br>`  channel: next-major` | Stable (next-major channel)         | `2.0.0` on `next-major` dist-tag |
| `- name: beta`<br>`  prerelease: beta`          | Prerelease (fixed channel)          | `2.0.0-beta.1`, `2.0.0-beta.2`   |
| `- name: "test-*"`<br>`  prerelease: true`      | Prerelease (branch name as channel) | `2.0.0-test-my-feature.1`        |
| `- name: "1.x"`<br>`  maintenance: true`        | Maintenance (major locked)          | `1.5.1`, `1.6.0` (no `2.x`)      |
| `- name: "1.5.x"`<br>`  maintenance: true`      | Maintenance (major+minor locked)    | `1.5.1`, `1.5.2` (no `1.6.x`)    |

##### Multiple release branches

You can have multiple stable release branches (e.g. `main`, `next`, `next-major`) that release independently. Each
non-primary branch should set a `channel` so it publishes to a different npm dist-tag:

```yaml
branches:
  - main                          # primary: publishes to "latest"
  - name: next
    channel: next                 # publishes to "next" dist-tag
  - name: next-major
    channel: next-major           # publishes to "next-major" dist-tag
```

**Version collision detection**: If a branch tries to release a version that already exists as a tag (e.g. `next`
released `1.1.0` and `main` also tries `1.1.0`), super-release will error. Merge the higher branch into the lower one
first, or let the lower branch release a different version.

##### Maintenance branches

Maintenance branches cap version bumps to stay within a range inferred from the branch name:

- `1.x` -- major is locked: `feat:` bumps minor, `feat!:` is capped to minor, no major bumps
- `1.5.x` -- major and minor are locked: all bumps become patch only

If the branch name doesn't follow the `N.x` / `N.N.x` pattern, set `range` explicitly:

```yaml
branches:
  - name: legacy-support
    maintenance: true
    range: "1.5.x"          # cap to 1.5.x patch range
```

In monorepos, packages whose version is outside the maintenance range are automatically skipped. For example, on branch
`1.x`, a package at `v3.0.0` will be skipped while a package at `v1.2.0` will be released normally.

##### Branch options

Branches can filter which packages they release with `packages`:

```yaml
branches:
  - name: "test-*"
    prerelease: true
    packages: # only release these on test branches
      - "@acme/core"
      - "@acme/utils"
```

**Tag filtering by branch**: Stable branches only see stable tags. Prerelease branches see their own channel's tags plus
stable tags. Tags on other branches that haven't been merged are ignored.

Default: `["main", "master"]`

#### `tag_format` / `tag_format_package`

Templates for git tag names. Placeholders: `{version}`, `{name}`.

```yaml
tag_format: "v{version}"                  # root: v1.2.3 (default)
tag_format_package: "{name}/v{version}"   # sub-packages: @acme/core/v1.2.3 (default)
tag_format_package: "{name}@{version}"    # semantic-release compat
```

#### `dependencies`

Global file dependency patterns. When a commit changes any matching file, ALL packages are considered affected.

```yaml
dependencies:
  - yarn.lock
  - pnpm-lock.yaml
  - package.json
  - ".github/**"
```

#### `ignore`

Glob patterns for files to ignore. Commits that only touch ignored files will not trigger a release. If a commit touches
both ignored and non-ignored files, only the non-ignored files determine which packages are affected.

```yaml
ignore:
  - "README.md"
  - "docs/**"
  - "**/*.md"
  - ".prettierrc"
```

#### `packages` / `exclude`

Filter which packages are released. `packages` is an allow-list (glob patterns), `exclude` is a deny-list.

```yaml
packages:
  - "@acme/*"
exclude:
  - my-monorepo-root
```

#### `steps`

Ordered list of steps. Each step has a `name`, optional `packages` and `branches` filters (glob patterns), and
`options`.

```yaml
steps:
  - name: changelog
    options:
      filename: CHANGELOG.md
      preview_lines: 20

  - name: npm
    packages: [ "@acme/*" ]       # only publish @acme packages
    branches: [ "main", "beta" ]  # only run on main and beta branches
    options:
      access: public
      provenance: true
      registry: https://registry.npmjs.org
      tag: next                 # dist-tag (default: auto from prerelease channel)
      publish_args: [ "--otp=123456" ]
      package_manager: yarn     # force specific PM (default: auto-detect)
      check_registry: true      # check if version exists before publishing (default: true)

  - name: exec
    packages: [ "my-rust-lib" ]
    options:
      prepare_cmd: "sed -i'' -e 's/^version = .*/version = \"{version}\"/' Cargo.toml"
      publish_cmd: "cargo publish"
      files: [ Cargo.toml, Cargo.lock ]   # include in git commit
```

Each step can be scoped:

- **`packages`** -- glob patterns to filter which packages the step operates on. If empty, the step runs for all
  packages. For example, `packages: ["@acme/*"]` limits an npm publish step to only `@acme`-scoped packages.
- **`branches`** -- glob patterns for branch names this step runs on. If empty, the step runs on all branches.
  For example, `branches: ["main"]` ensures a step only runs on the main branch.

| Step        | Prepare                                            | Publish                                                |
|-------------|----------------------------------------------------|--------------------------------------------------------|
| `changelog` | Generates/updates changelog per package (parallel) | --                                                     |
| `npm`       | --                                                 | Publishes packages (parallel within dependency levels) |
| `exec`      | Runs custom shell command per package              | Runs custom shell command per package                  |

Package version bumps (`package.json`) happen automatically before steps run (part of core).
Steps return the files they modified. The core git step stages exactly those files for the commit -- no `git add .`.

Default: `[changelog, npm]`

#### `git`

Core git behavior after all steps run. Not a step -- always runs.

```yaml
git:
  commit_message: "chore(release): {releases} [skip ci]"
  push: false          # push commit + tags to remote
  remote: origin
```

Commit message placeholders:

- `{releases}` -- comma-separated: `@acme/core@1.1.0, @acme/utils@1.0.1`
- `{summary}` -- one per line: `  - @acme/core 1.0.0 -> 1.1.0`
- `{count}` -- number of packages released

The git step:

1. Stages files reported by steps (changelogs, exec `files`, package.json bumps)
2. Commits (or skips if nothing changed)
3. Creates annotated tags for each release
4. Pushes commit + tags if `push: true`

Tags are idempotent -- existing tags are skipped. The npm step checks the registry before publishing (`npm view`) and
skips versions that already exist. Non-404 errors (auth, network) abort the release to prevent partial publishes.

## Monorepo Support

### Structure

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

Packages are discovered by finding `package.json` files recursively (respects `.gitignore`). Each commit is associated
to a package based on which files it changed.

### Independent versioning

Each package has its own version and release tag. A commit that only touches `packages/core/` will only bump
`@acme/core`. Packages are versioned independently -- `@acme/core` can be at `v3.0.0` while `@acme/utils` is at
`v1.2.0`.

### Filtering packages

Use `packages` (allow-list) and `exclude` (deny-list) at the top level to control which packages are released:

```yaml
packages:
  - "@acme/*"        # only release @acme-scoped packages
exclude:
  - my-monorepo-root # skip the root package
```

### Dependencies and publish order

Packages that depend on each other (via `dependencies` or `devDependencies` in `package.json`) are published in
dependency order. If `@acme/utils` depends on `@acme/core`, core is published first. Independent packages publish in
parallel.

### Global file dependencies

Files that affect all packages (lock files, shared config) can be declared as global dependencies. A commit that only
changes `yarn.lock` will trigger releases for all packages:

```yaml
dependencies:
  - yarn.lock
  - pnpm-lock.yaml
```

### Maintenance branches in monorepos

On a maintenance branch like `1.x`, packages whose current version is outside the maintenance range are automatically
skipped. For example, if `@acme/core` is at `v3.0.0` and `@acme/utils` is at `v1.2.0`, only `@acme/utils` will be
released on the `1.x` branch.

You can also use per-branch `packages` filters for explicit control:

```yaml
branches:
  - name: "1.x"
    maintenance: true
    packages:
      - "@acme/utils"    # only release utils on this maintenance branch
```

## Performance

- **Tag-bounded history walk**: only walks commits since the oldest package tag
- **Single-pass commit collection**: commits fetched once, partitioned per package
- **Reachable-only tags**: single revwalk to check tag reachability, stops early

## Acknowledgements

super-release is inspired by and builds on the ideas of:

- **[semantic-release](https://github.com/semantic-release/semantic-release)** -- the original automated release tool
  that pioneered conventional-commit-based versioning
- **[git-cliff](https://github.com/orhun/git-cliff)** -- powers changelog generation via `git-cliff-core`

## License

[MIT](LICENSE)
