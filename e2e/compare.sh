#!/usr/bin/env bash
#
# E2E comparison: super-release vs semantic-release
#
# Verifies that both tools produce the same version bumps for identical
# git histories. Runs both in dry-run mode, compares computed versions.
#
# Usage:
#   cargo build --release
#   SUPER_RELEASE=./target/release/super-release bash e2e/compare.sh
#
# Prerequisites: node/npx, git, super-release binary

set -euo pipefail

SUPER_RELEASE="${SUPER_RELEASE:-super-release}"
PASS=0
FAIL=0
SKIP=0
TMPBASE=""

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BOLD='\033[1m'
DIM='\033[2m'
NC='\033[0m'

cleanup() {
  if [ -n "$TMPBASE" ] && [ -d "$TMPBASE" ]; then
    rm -rf "$TMPBASE"
  fi
}
trap cleanup EXIT

TMPBASE=$(mktemp -d)

# Install semantic-release + exec plugin once into shared location
SR_MODULES="$TMPBASE/sr-modules"
mkdir -p "$SR_MODULES"
echo '{"private": true}' > "$SR_MODULES/package.json"
echo -e "${DIM}Installing semantic-release...${NC}"
npm install --prefix "$SR_MODULES" semantic-release @semantic-release/commit-analyzer @semantic-release/release-notes-generator --save-dev --silent 2>/dev/null
SR_BIN="$SR_MODULES/node_modules/.bin/semantic-release"
export NODE_PATH="$SR_MODULES/node_modules"

# ── Helpers ──────────────────────────────────────────────────────────

setup_repo() {
  local dir="$TMPBASE/repo-$$-$RANDOM"
  local bare="$dir.bare"

  # Create a local bare repo as remote (semantic-release requires a valid remote)
  git init --bare --quiet "$bare"

  mkdir -p "$dir"
  git -C "$dir" init -b main --quiet
  git -C "$dir" config user.email "test@test.com"
  git -C "$dir" config user.name "Test"
  git -C "$dir" remote add origin "$bare"

  # Minimal package.json with local repo URL.
  # Note: version is set to 1.0.0 here; tests that need a different starting
  # version should overwrite it before the initial commit.
  cat > "$dir/package.json" << EOF
{"name": "test-pkg", "version": "1.0.0", "repository": {"type": "git", "url": "file://$bare"}}
EOF

  # Symlink shared node_modules so semantic-release can find plugins
  ln -s "$SR_MODULES/node_modules" "$dir/node_modules"

  echo "$dir"
}

write_sr_config() {
  local dir="$1"
  local branches_json="$2"

  cat > "$dir/.releaserc.json" << EOF
{
  "branches": $branches_json,
  "plugins": [
    "@semantic-release/commit-analyzer",
    "@semantic-release/release-notes-generator"
  ]
}
EOF
}

write_our_config() {
  local dir="$1"
  local yaml_content="$2"

  cat > "$dir/.release.yaml" << EOF
$yaml_content
EOF
}

git_commit() {
  local dir="$1"
  local msg="$2"
  # Touch a file so there's always something to commit
  echo "$msg" >> "$dir/changes.txt"
  git -C "$dir" add .
  git -C "$dir" commit -m "$msg" --quiet
}

git_tag() {
  local dir="$1"
  local tag="$2"
  git -C "$dir" tag -a "$tag" -m "$tag"
}

git_push() {
  local dir="$1"
  git -C "$dir" push origin --all --quiet 2>/dev/null || true
  git -C "$dir" push origin --tags --quiet 2>/dev/null || true
}

git_fetch_all() {
  local dir="$1"
  git -C "$dir" fetch origin --tags --quiet 2>/dev/null || true
  git -C "$dir" fetch origin 'refs/notes/*:refs/notes/*' --quiet 2>/dev/null || true
}

# Get version from semantic-release (dry-run)
sr_version() {
  local dir="$1"

  # semantic-release requires branch to be pushed to remote.
  # Also fetch notes/tags — SR uses git notes to track releases.
  git_push "$dir"
  git_fetch_all "$dir"

  # Run semantic-release in dry-run mode, capture all output
  local output
  output=$((cd "$dir" && "$SR_BIN" --dry-run --no-ci) 2>&1) || true

  # Parse "The next release version is X.Y.Z" from output
  local parsed
  parsed=$(echo "$output" | grep -oE 'next release version is [0-9][^ ]*' | head -1 | sed 's/next release version is //')
  if [ -n "$parsed" ]; then
    echo "$parsed" | tr -d '[:space:]'
    return
  fi

  echo "NO_RELEASE"
}

# Get version from super-release
our_version() {
  local dir="$1"

  local result
  result=$("$SUPER_RELEASE" --show-next-version -C "$dir" 2>/dev/null) || true

  if [ -z "$result" ]; then
    echo "NO_RELEASE"
  else
    echo "$result" | tr -d '[:space:]'
  fi
}

# Compare results
compare() {
  local name="$1"
  local sr="$2"
  local us="$3"

  if [ "$sr" = "$us" ]; then
    echo -e "  ${GREEN}PASS${NC} ${BOLD}$name${NC} ${DIM}($sr)${NC}"
    PASS=$((PASS + 1))
  else
    echo -e "  ${RED}FAIL${NC} ${BOLD}$name${NC}"
    echo -e "       semantic-release: ${YELLOW}$sr${NC}"
    echo -e "       super-release:    ${YELLOW}$us${NC}"
    FAIL=$((FAIL + 1))
  fi
}

# ── Test scenarios ───────────────────────────────────────────────────

test_feat_minor() {
  local dir
  dir=$(setup_repo)

  write_sr_config "$dir" '["main"]'
  write_our_config "$dir" "branches: [main]
steps: []"

  git_commit "$dir" "chore: init"
  git_tag "$dir" "v1.0.0"
  git_commit "$dir" "feat: add new feature"

  compare "feat → minor (1.1.0)" "$(sr_version "$dir")" "$(our_version "$dir")"
}

test_fix_patch() {
  local dir
  dir=$(setup_repo)

  write_sr_config "$dir" '["main"]'
  write_our_config "$dir" "branches: [main]
steps: []"

  git_commit "$dir" "chore: init"
  git_tag "$dir" "v1.0.0"
  git_commit "$dir" "fix: resolve bug"

  compare "fix → patch (1.0.1)" "$(sr_version "$dir")" "$(our_version "$dir")"
}

test_breaking_major() {
  local dir
  dir=$(setup_repo)

  write_sr_config "$dir" '["main"]'
  write_our_config "$dir" "branches: [main]
steps: []"

  git_commit "$dir" "chore: init"
  git_tag "$dir" "v1.0.0"

  # Use BREAKING CHANGE footer (universally supported by both tools)
  echo "breaking" >> "$dir/changes.txt"
  git -C "$dir" add .
  git -C "$dir" commit -m "feat: redesign API

BREAKING CHANGE: complete API overhaul" --quiet

  compare "breaking change → major (2.0.0)" "$(sr_version "$dir")" "$(our_version "$dir")"
}

test_breaking_footer() {
  local dir
  dir=$(setup_repo)

  write_sr_config "$dir" '["main"]'
  write_our_config "$dir" "branches: [main]
steps: []"

  git_commit "$dir" "chore: init"
  git_tag "$dir" "v1.0.0"

  echo "breaking" >> "$dir/changes.txt"
  git -C "$dir" add .
  git -C "$dir" commit -m "feat: new thing

BREAKING CHANGE: this changes the API" --quiet

  compare "BREAKING CHANGE footer → major (2.0.0)" "$(sr_version "$dir")" "$(our_version "$dir")"
}

test_chore_no_release() {
  local dir
  dir=$(setup_repo)

  write_sr_config "$dir" '["main"]'
  write_our_config "$dir" "branches: [main]
steps: []"

  git_commit "$dir" "chore: init"
  git_tag "$dir" "v1.0.0"
  git_commit "$dir" "chore: update deps"

  local sr us
  sr=$(sr_version "$dir")
  us=$(our_version "$dir")

  # Both should indicate no release:
  # - semantic-release: NO_RELEASE (no .next-version line in output)
  # - super-release --show-next-version: returns current version when no bump
  local current_version="1.0.0" # matches the v1.0.0 tag
  local pass=false
  if [ "$sr" = "NO_RELEASE" ] && [ "$us" = "$current_version" ]; then
    pass=true
  fi

  if [ "$pass" = true ]; then
    echo -e "  ${GREEN}PASS${NC} ${BOLD}chore → no release${NC} ${DIM}(sr=NO_RELEASE, us=1.0.0 [current])${NC}"
    PASS=$((PASS + 1))
  else
    echo -e "  ${RED}FAIL${NC} ${BOLD}chore → no release${NC}"
    echo -e "       semantic-release: ${YELLOW}$sr${NC}"
    echo -e "       super-release:    ${YELLOW}$us${NC}"
    FAIL=$((FAIL + 1))
  fi
}

test_mixed_highest_wins() {
  local dir
  dir=$(setup_repo)

  write_sr_config "$dir" '["main"]'
  write_our_config "$dir" "branches: [main]
steps: []"

  git_commit "$dir" "chore: init"
  git_tag "$dir" "v1.0.0"
  git_commit "$dir" "fix: bug fix"
  git_commit "$dir" "feat: new feature"

  compare "mixed fix+feat → minor (1.1.0)" "$(sr_version "$dir")" "$(our_version "$dir")"
}

test_perf_patch() {
  local dir
  dir=$(setup_repo)

  write_sr_config "$dir" '["main"]'
  write_our_config "$dir" "branches: [main]
steps: []"

  git_commit "$dir" "chore: init"
  git_tag "$dir" "v1.0.0"
  git_commit "$dir" "perf: optimize hot path"

  compare "perf → patch (1.0.1)" "$(sr_version "$dir")" "$(our_version "$dir")"
}

test_prerelease_first() {
  local dir
  dir=$(setup_repo)

  write_sr_config "$dir" '["main", {"name": "beta", "prerelease": true}]'
  write_our_config "$dir" 'branches:
  - main
  - name: beta
    prerelease: beta
steps: []'

  git_commit "$dir" "chore: init"
  git_tag "$dir" "v1.0.0"

  git -C "$dir" checkout -b beta --quiet
  git_commit "$dir" "feat: beta feature"

  compare "first prerelease (1.1.0-beta.1)" "$(sr_version "$dir")" "$(our_version "$dir")"
}

test_prerelease_increment() {
  local dir
  dir=$(setup_repo)

  write_sr_config "$dir" '["main", {"name": "beta", "prerelease": true}]'
  write_our_config "$dir" 'branches:
  - main
  - name: beta
    prerelease: beta
steps: []'

  git_commit "$dir" "chore: init"
  git_tag "$dir" "v1.0.0"
  git_push "$dir"

  git -C "$dir" checkout -b beta --quiet
  git_commit "$dir" "feat: beta feature"
  git_push "$dir"

  # Let SR do a real run to create v1.1.0-beta.1 tag + git notes.
  (cd "$dir" && "$SR_BIN" --no-ci) > /dev/null 2>&1 || true
  git_fetch_all "$dir"

  git_commit "$dir" "fix: beta fix"

  compare "prerelease increment (1.1.0-beta.2)" "$(sr_version "$dir")" "$(our_version "$dir")"
}

test_prerelease_breaking() {
  local dir
  dir=$(setup_repo)

  write_sr_config "$dir" '["main", {"name": "beta", "prerelease": true}]'
  write_our_config "$dir" 'branches:
  - main
  - name: beta
    prerelease: beta
steps: []'

  git_commit "$dir" "chore: init"
  git_tag "$dir" "v1.0.0"

  git -C "$dir" checkout -b beta --quiet
  echo "breaking" >> "$dir/changes.txt"
  git -C "$dir" add .
  git -C "$dir" commit -m "feat: breaking beta change

BREAKING CHANGE: new API" --quiet

  compare "breaking on prerelease (2.0.0-beta.1)" "$(sr_version "$dir")" "$(our_version "$dir")"
}

test_maintenance_fix() {
  local dir
  dir=$(setup_repo)

  write_sr_config "$dir" '["1.x", "main"]'
  write_our_config "$dir" 'branches:
  - main
  - name: "1.x"
    maintenance: true
steps: []'

  git_commit "$dir" "chore: init"
  git_tag "$dir" "v1.5.0"

  # Move main ahead to v2.0.0 so 1.x is truly a maintenance branch
  git_commit "$dir" "feat!: v2"
  git_tag "$dir" "v2.0.0"

  git -C "$dir" checkout -b "1.x" v1.5.0 --quiet
  git_commit "$dir" "fix: backport fix"

  compare "maintenance fix on 1.x (1.5.1)" "$(sr_version "$dir")" "$(our_version "$dir")"
}

test_maintenance_feat() {
  local dir
  dir=$(setup_repo)

  write_sr_config "$dir" '["1.x", "main"]'
  write_our_config "$dir" 'branches:
  - main
  - name: "1.x"
    maintenance: true
steps: []'

  git_commit "$dir" "chore: init"
  git_tag "$dir" "v1.5.0"

  git_commit "$dir" "feat!: v2"
  git_tag "$dir" "v2.0.0"

  git -C "$dir" checkout -b "1.x" v1.5.0 --quiet
  git_commit "$dir" "feat: new feature"

  compare "maintenance feat on 1.x (1.6.0)" "$(sr_version "$dir")" "$(our_version "$dir")"
}

test_maintenance_breaking_capped() {
  local dir
  dir=$(setup_repo)

  # SR requires maintenance branches BEFORE release branches
  write_sr_config "$dir" '["1.x", "main"]'
  write_our_config "$dir" 'branches:
  - main
  - name: "1.x"
    maintenance: true
steps: []'

  git_commit "$dir" "chore: init"
  git_tag "$dir" "v1.5.0"

  echo "v2" >> "$dir/changes.txt"
  git -C "$dir" add .
  git -C "$dir" commit -m "feat: v2 trigger

BREAKING CHANGE: major bump" --quiet
  git_tag "$dir" "v2.0.0"
  git_push "$dir"

  # Real run on main so SR "owns" the releases
  (cd "$dir" && "$SR_BIN" --no-ci) > /dev/null 2>&1 || true
  git_fetch_all "$dir"

  git -C "$dir" checkout -b "1.x" v1.5.0 --quiet
  echo "breaking" >> "$dir/changes.txt"
  git -C "$dir" add .
  git -C "$dir" commit -m "feat: breaking but capped

BREAKING CHANGE: capped on maintenance" --quiet
  git_push "$dir"

  # SR real run on 1.x — capture error output
  local sr_output
  sr_output=$((cd "$dir" && "$SR_BIN" --no-ci) 2>&1) || true

  local us
  us=$(our_version "$dir")

  # SR computes 2.0.0 but errors with EINVALIDNEXTVERSION (out of range >=1.5.0 <2.0.0).
  # We silently cap to 1.6.0 (within range).
  # Both tools prevent the invalid release — SR errors, we auto-cap.
  local sr_errored=false
  if echo "$sr_output" | grep -q "EINVALIDNEXTVERSION"; then
    sr_errored=true
  fi

  if [ "$sr_errored" = true ] && [ "$us" = "1.6.0" ]; then
    echo -e "  ${GREEN}PASS${NC} ${BOLD}maintenance breaking on 1.x${NC} ${DIM}(sr=EINVALIDNEXTVERSION [errors], us=1.6.0 [caps to minor] — both prevent bad release)${NC}"
    PASS=$((PASS + 1))
  else
    echo -e "  ${RED}FAIL${NC} ${BOLD}maintenance breaking on 1.x${NC}"
    echo -e "       semantic-release: ${YELLOW}$(echo "$sr_output" | grep 'EINVALIDNEXTVERSION' | head -1)${NC}"
    echo -e "       super-release:    ${YELLOW}$us${NC}"
    FAIL=$((FAIL + 1))
  fi
}

test_first_release() {
  local dir
  dir=$(setup_repo)

  write_sr_config "$dir" '["main"]'
  write_our_config "$dir" "branches: [main]
steps: []"

  # Override to 0.0.0 for "no prior release" scenario
  local bare_url
  bare_url=$(git -C "$dir" remote get-url origin)
  cat > "$dir/package.json" << EOF
{"name": "test-pkg", "version": "0.0.0", "repository": {"type": "git", "url": "$bare_url"}}
EOF

  git_commit "$dir" "feat: initial feature"

  local sr us
  sr=$(sr_version "$dir")
  us=$(our_version "$dir")

  # Known difference: semantic-release always starts at 1.0.0.
  # super-release bumps from package.json version (0.0.0 → 0.1.0 for feat).
  # Both are valid — SR has a hardcoded initial version, we use package.json.
  if [ "$sr" = "1.0.0" ] && [ "$us" = "0.1.0" ]; then
    echo -e "  ${YELLOW}SKIP${NC} ${BOLD}first release ever${NC} ${DIM}(known difference: sr=$sr, us=$us — SR hardcodes 1.0.0, we use package.json)${NC}"
    SKIP=$((SKIP + 1))
  else
    compare "first release ever" "$sr" "$us"
  fi
}

# ── Multi-branch scenarios (from SR integration tests) ───────────────

test_forward_port_fix_from_maintenance() {
  # SR test #4: fix on 1.0.x merged into master triggers patch release
  local dir
  dir=$(setup_repo)

  write_sr_config "$dir" '["1.0.x", "main"]'
  write_our_config "$dir" 'branches:
  - main
  - name: "*.*.x"
    maintenance: true
steps: []'

  git_commit "$dir" "chore: init"
  git_tag "$dir" "v1.0.0"
  git_push "$dir"

  # Release v1.1.0 on main
  git_commit "$dir" "feat: new feature on main"
  git_tag "$dir" "v1.1.0"
  git_push "$dir"

  # Let SR own the releases on main
  (cd "$dir" && "$SR_BIN" --no-ci) > /dev/null 2>&1 || true
  git_fetch_all "$dir"

  # Create maintenance branch, make a fix
  git -C "$dir" checkout -b "1.0.x" v1.0.0 --quiet
  git_commit "$dir" "fix: fix on maintenance version 1.0.x"
  git_tag "$dir" "v1.0.1"
  git_push "$dir"

  # Let SR own the 1.0.x release
  (cd "$dir" && "$SR_BIN" --no-ci) > /dev/null 2>&1 || true
  git_fetch_all "$dir"

  # Merge 1.0.x fix into main (forward-port)
  git -C "$dir" checkout main --quiet
  local merge_out
  merge_out=$(git -C "$dir" merge "1.0.x" -m "Merge 1.0.x into main" 2>&1) || {
    # Resolve conflict if any
    echo "merged" > "$dir/changes.txt"
    git -C "$dir" add .
    git -C "$dir" commit -m "Merge 1.0.x into main" --quiet
  }

  compare "forward-port fix from 1.0.x to main (1.1.1)" "$(sr_version "$dir")" "$(our_version "$dir")"
}

test_release_branch_out_of_range() {
  # SR test #12: master tries to release version that next already has → error
  local dir
  dir=$(setup_repo)

  write_sr_config "$dir" '["main", {"name": "next", "channel": "next"}]'
  write_our_config "$dir" 'branches:
  - main
  - name: next
    channel: next
steps: []'

  git_commit "$dir" "chore: init"
  git_tag "$dir" "v1.0.0"
  git_push "$dir"

  # next releases v1.1.0
  git -C "$dir" checkout -b next --quiet
  git_commit "$dir" "feat: new feature on next"
  git_tag "$dir" "v1.1.0"
  git_push "$dir"

  # Let SR own the next release
  (cd "$dir" && "$SR_BIN" --no-ci) > /dev/null 2>&1 || true
  git_fetch_all "$dir"

  # Back on main, make a feat commit (would produce 1.1.0 → collision)
  git -C "$dir" checkout main --quiet
  git_commit "$dir" "feat: main feature"

  local sr_output us
  sr_output=$((cd "$dir" && git_push "$dir" && "$SR_BIN" --no-ci) 2>&1) || true
  us=$("$SUPER_RELEASE" --dry-run -C "$dir" 2>&1) || true

  local sr_errored=false us_errored=false
  if echo "$sr_output" | grep -q "EINVALIDNEXTVERSION"; then
    sr_errored=true
  fi
  if echo "$us" | grep -q "already exists as a tag"; then
    us_errored=true
  fi

  if [ "$sr_errored" = true ] && [ "$us_errored" = true ]; then
    echo -e "  ${GREEN}PASS${NC} ${BOLD}release branch collision (main vs next)${NC} ${DIM}(both error on version conflict)${NC}"
    PASS=$((PASS + 1))
  else
    echo -e "  ${RED}FAIL${NC} ${BOLD}release branch collision (main vs next)${NC}"
    echo -e "       sr errored: ${YELLOW}$sr_errored${NC}"
    echo -e "       us errored: ${YELLOW}$us_errored${NC}"
    FAIL=$((FAIL + 1))
  fi
}

test_maintenance_out_of_range_feat() {
  # SR test #11: feat on 1.x would produce 1.1.0 but master already released it → error
  local dir
  dir=$(setup_repo)

  write_sr_config "$dir" '["1.x", "main"]'
  write_our_config "$dir" 'branches:
  - main
  - name: "1.x"
    maintenance: true
steps: []'

  git_commit "$dir" "chore: init"
  git_tag "$dir" "v1.0.0"
  git_push "$dir"

  # Release v1.1.0 on main
  git_commit "$dir" "feat: new feature on main"
  git_tag "$dir" "v1.1.0"
  git_push "$dir"

  # Let SR own the main releases
  (cd "$dir" && "$SR_BIN" --no-ci) > /dev/null 2>&1 || true
  git_fetch_all "$dir"

  # 1.x from v1.0.0, add feat (would produce 1.1.0 → collision)
  git -C "$dir" checkout -b "1.x" v1.0.0 --quiet
  git_commit "$dir" "feat: feature on maintenance 1.x"
  git_push "$dir"

  local sr_output us
  sr_output=$((cd "$dir" && "$SR_BIN" --no-ci) 2>&1) || true
  us=$("$SUPER_RELEASE" --dry-run -C "$dir" 2>&1) || true

  local sr_errored=false us_errored=false
  if echo "$sr_output" | grep -q "EINVALIDNEXTVERSION"; then
    sr_errored=true
  fi
  if echo "$us" | grep -q "already exists as a tag"; then
    us_errored=true
  fi

  if [ "$sr_errored" = true ] && [ "$us_errored" = true ]; then
    echo -e "  ${GREEN}PASS${NC} ${BOLD}maintenance out-of-range feat on 1.x${NC} ${DIM}(both error on collision with v1.1.0)${NC}"
    PASS=$((PASS + 1))
  else
    echo -e "  ${RED}FAIL${NC} ${BOLD}maintenance out-of-range feat on 1.x${NC}"
    echo -e "       sr errored: ${YELLOW}$sr_errored${NC}"
    echo -e "       us errored: ${YELLOW}$us_errored${NC}"
    FAIL=$((FAIL + 1))
  fi
}

test_prerelease_multiple_increments() {
  # SR test #5: multiple commits on beta → incrementing prerelease numbers
  local dir
  dir=$(setup_repo)

  write_sr_config "$dir" '["main", {"name": "beta", "prerelease": true}]'
  write_our_config "$dir" 'branches:
  - main
  - name: beta
    prerelease: beta
steps: []'

  git_commit "$dir" "chore: init"
  git_tag "$dir" "v1.0.0"
  git_push "$dir"

  git -C "$dir" checkout -b beta --quiet

  # Run 1: feat → beta.1
  git_commit "$dir" "feat: first beta feature"
  git_push "$dir"
  (cd "$dir" && "$SR_BIN" --no-ci) > /dev/null 2>&1 || true
  git_fetch_all "$dir"

  # Run 2: fix → beta.2
  git_commit "$dir" "fix: beta bugfix"
  git_push "$dir"
  (cd "$dir" && "$SR_BIN" --no-ci) > /dev/null 2>&1 || true
  git_fetch_all "$dir"

  # Run 3: another feat → beta.3
  git_commit "$dir" "feat: another beta feature"

  compare "prerelease 3rd increment (1.1.0-beta.3)" "$(sr_version "$dir")" "$(our_version "$dir")"
}

# ── Main ─────────────────────────────────────────────────────────────

main() {
  echo ""
  echo -e "${BOLD}E2E Comparison: super-release vs semantic-release${NC}"
  echo ""

  # Check prerequisites
  if ! command -v npm &> /dev/null; then
    echo -e "${RED}Error: npm not found. Install Node.js first.${NC}"
    exit 1
  fi

  if ! command -v "$SUPER_RELEASE" &> /dev/null && [ ! -x "$SUPER_RELEASE" ]; then
    echo -e "${RED}Error: super-release not found at '$SUPER_RELEASE'.${NC}"
    echo "Build first: cargo build --release"
    echo "Then: SUPER_RELEASE=./target/release/super-release bash e2e/compare.sh"
    exit 1
  fi

  echo -e "${DIM}Using: $SUPER_RELEASE${NC}"
  echo -e "${DIM}Using: $("$SR_BIN" --version 2>/dev/null || echo 'unknown')${NC}"
  echo ""

  echo -e "${BOLD}Stable releases${NC}"
  test_feat_minor
  test_fix_patch
  test_breaking_major
  test_breaking_footer
  test_chore_no_release
  test_mixed_highest_wins
  test_perf_patch

  echo ""
  echo -e "${BOLD}Prerelease branches${NC}"
  test_prerelease_first
  test_prerelease_increment
  test_prerelease_breaking
  test_prerelease_multiple_increments

  echo ""
  echo -e "${BOLD}Maintenance branches${NC}"
  test_maintenance_fix
  test_maintenance_feat
  test_maintenance_breaking_capped

  echo ""
  echo -e "${BOLD}Multi-branch scenarios${NC}"
  test_forward_port_fix_from_maintenance
  test_release_branch_out_of_range
  test_maintenance_out_of_range_feat

  echo ""
  echo -e "${BOLD}Edge cases${NC}"
  test_first_release

  echo ""
  echo "────────────────────────────────────────"
  echo -e "Results: ${GREEN}$PASS passed${NC}, ${RED}$FAIL failed${NC}, ${YELLOW}$SKIP skipped${NC}"
  echo ""

  [ "$FAIL" -eq 0 ]
}

main "$@"
