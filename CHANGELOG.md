# Changelog

## [1.3.1] - 2026-04-06

### 🐛 Bug Fixes

- Make sure files are executable
## [1.3.0] - 2026-04-06

### 🚀 Features

- Adjusted packaging
- Adjusted packaging
- Package every arch separately

### 🐛 Bug Fixes

- Exclude root package
- Make sure to install all debs
- Adjusted path's
- Adjusted repo structure

### ⚙️ Miscellaneous Tasks

- Bump
- *(release)* Super-release-windows-x64@1.1.0, super-release-linux-arm64@1.1.0, super-release-darwin-arm64@1.1.0, super-release@1.3.0, super-release-darwin-x64@1.1.0, super-release-linux-x64@1.1.0 [skip ci]
## [1.2.0] - 2026-04-04

### 🚀 Features

- Add visibility for implicitly skipped packages, ci/cd

### ⚙️ Miscellaneous Tasks

- Configured toolchain
- Adjusted ci/cd, added dependabot
- Formatting, added more tests for monorepos
## [1.1.4] - 2026-04-04

### 🐛 Bug Fixes

- Validate build hash before executing binary

### ⚙️ Miscellaneous Tasks

- Added lockfile
## [1.1.3] - 2026-04-01

### 🐛 Bug Fixes

- Restructured ci/cd

### ⚙️ Miscellaneous Tasks

- Debug
- Debug
- Debug
- Force `main` as branch
- Force `main` as branch
- Run tests in parallel, fixed output, stream results
- Add failure outputs
- Enforce CI=false on gh action runners as well
## [1.1.2] - 2026-04-01

### 🐛 Bug Fixes

- Some cleanup / refactoring
- Fail on invalid / missing maintenance branch ranges
## [1.1.1] - 2026-04-01

### 🐛 Bug Fixes

- Simplify verbose logging

### ⚙️ Miscellaneous Tasks

- Added 2e2 comparison
## [1.1.0] - 2026-04-01

### 🚀 Features

- Added more maintenance / channel tests and adjustments, added docs for different scenarios
- Added version collision tests / handling
## [1.0.3] - 2026-04-01

### 🐛 Bug Fixes

- Make sure to handle prerelease cutoff correctly
## [1.0.2] - 2026-04-01

### 🐛 Bug Fixes

- Simplifications
## [1.0.1] - 2026-04-01

### 🐛 Bug Fixes

- Make sure not to rewrite the `package.json` files and preserve formatting
## [1.0.0] - 2026-04-01

### 🚀 Features

- [**breaking**] Added support for json and json config files, cleanup
## [0.15.0] - 2026-04-01

### 🚀 Features

- Better npm precheck error handling, updated docs
## [0.14.0] - 2026-04-01

### 🚀 Features

- Run publish checks in parallel
## [0.13.3] - 2026-04-01

### 🐛 Bug Fixes

- Adjusted checks, more output
## [0.13.2] - 2026-04-01

### 🐛 Bug Fixes

- Made output clearer
## [0.13.1] - 2026-04-01

### 🐛 Bug Fixes

- Removed invalid `--registry` argument in publish command

### ⚙️ Miscellaneous Tasks

- Added test for npm skip publish case
## [0.13.0] - 2026-04-01

### 🚀 Features

- Test if the version is already published before publishing
## [0.12.0] - 2026-04-01

### 🚀 Features

- Show multi progress for concurrent builds
## [0.11.0] - 2026-04-01

### 🚀 Features

- Add package include filter for branches
## [0.10.0] - 2026-03-31

### 🚀 Features

- Add support for musl
## [0.9.0] - 2026-03-31

### 🚀 Features

- Simplified & optimized using `git log`, fixed config file handling
## [0.8.0] - 2026-03-31

### 🚀 Features

- Review & performance improvements
## [0.7.0] - 2026-03-31

### 🚀 Features

- Review & performance improvements

### ⚙️ Miscellaneous Tasks

- Format
## [0.6.0] - 2026-03-31

### 🚀 Features

- Refactored command exec, make clearer in what folder the commands are executed
## [0.5.1] - 2026-03-31

### 🐛 Bug Fixes

- Moved package update to the right location, added info about package includes/excludes
## [0.5.0] - 2026-03-31

### 🚀 Features

- Cleanup, add global ignore/dependency pattern, make git integration global

### ⚙️ Miscellaneous Tasks

- Docs
## [0.4.0] - 2026-03-31

### 🚀 Features

- Communicate version properly, adjusted docs, add `--show-next-version`
## [0.3.1] - 2026-03-31

### 🐛 Bug Fixes

- Escape properly
## [0.3.0] - 2026-03-31

### 🚀 Features

- Added windows support, adjusted npm wrapper

### ⚙️ Miscellaneous Tasks

- Make sure not to bump version for no-bump commits
## [0.2.1] - 2026-03-31

### ⚙️ Miscellaneous Tasks

- Cleanup
- Removed artifacts, cleanup
## [0.2.0] - 2026-03-30

### 🚀 Features

- Added prerelease check
- Added schemas, adjusted release
- Added exec plugin
- Initial release
- Optimizations

### 🐛 Bug Fixes

- Fixed version
- Moved release package to root
- Added missing openssl
- Adjustments

### ⚙️ Miscellaneous Tasks

- Adjustments
- Adjustments
- Adjustments
- Adjustments
- Adjustments
- Removed token
- Explicitly define registry url
- Adjusted env
- Added token
- Added license
- Adjusted test glob match
- Bumped artifact versions
- Optimized command output, bumped node
- Bumped actions
- Initial release
- Cleanup
- Refactoring
- Refactoring
- Refactoring
- Skip release on unconfigured branch
- Adjustments
- Adjustments
- Glob adjustments
- Use rust 2024
- Initial commit
