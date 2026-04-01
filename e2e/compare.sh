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
PARALLEL="${PARALLEL:-4}"
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

# Force non-CI environment so semantic-release doesn't use GitHub-specific
# branch detection (e.g. GITHUB_REF) or require tokens.
for var in $(env | grep -oE '^GITHUB_[A-Z_]*' | cut -d= -f1); do
  unset "$var" 2>/dev/null || true
done
export CI=false

# ── Helpers ──────────────────────────────────────────────────────────

setup_repo() {
  local dir="$TMPBASE/repo-$$-$RANDOM"
  local bare="$dir.bare"

  # Create a local bare repo as remote (semantic-release requires a valid remote).
  git init --bare --quiet "$bare"
  # Ensure the bare repo's default branch is 'main' — CI runners may default to 'master',
  # which causes semantic-release to reject the branch.
  git -C "$bare" symbolic-ref HEAD refs/heads/main

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

# Results directory — each test writes its result to a file here
RESULTS_DIR="$TMPBASE/results"
mkdir -p "$RESULTS_DIR"

# Per-test globals (set by run_sr / run_ours, used by compare)
SR_VERSION=""
SR_LAST_OUTPUT=""
OUR_VERSION=""
OUR_LAST_OUTPUT=""

run_sr() {
  local dir="$1"
  git_push "$dir"
  git_fetch_all "$dir"

  SR_LAST_OUTPUT=$((cd "$dir" && "$SR_BIN" --dry-run --no-ci) 2>&1) || true

  local parsed
  parsed=$(echo "$SR_LAST_OUTPUT" | grep -oE 'next release version is [0-9][^ ]*' | head -1 | sed 's/next release version is //' || true)
  if [ -n "$parsed" ]; then
    SR_VERSION=$(echo "$parsed" | tr -d '[:space:]')
  else
    SR_VERSION="NO_RELEASE"
  fi
}

run_ours() {
  local dir="$1"
  OUR_LAST_OUTPUT=$("$SUPER_RELEASE" --show-next-version -C "$dir" 2>&1) || true

  if [ -z "$OUR_LAST_OUTPUT" ]; then
    OUR_VERSION="NO_RELEASE"
  else
    OUR_VERSION=$(echo "$OUR_LAST_OUTPUT" | tr -d '[:space:]')
  fi
}

# Write result to a file for later collection.
# Usage: report_result "test_name" "PASS|FAIL|SKIP" "detail line(s)"
report_result() {
  local name="$1" status="$2" detail="${3:-}"
  # Use a sanitized filename
  local fname
  fname=$(echo "$name" | tr ' /!()' '_____')
  echo "$status" > "$RESULTS_DIR/$fname"
  echo "$name" >> "$RESULTS_DIR/$fname"
  if [ -n "$detail" ]; then
    echo "$detail" >> "$RESULTS_DIR/$fname"
  fi
}

# Compare SR_VERSION vs OUR_VERSION and write result
compare() {
  local name="$1"

  if [ "$SR_VERSION" = "$OUR_VERSION" ]; then
    report_result "$name" "PASS" "$SR_VERSION"
  else
    local detail
    detail=$(printf "semantic-release: %s\nsuper-release:    %s\n── semantic-release output (last 20 lines) ──\n%s\n── super-release output (last 10 lines) ──\n%s" \
      "$SR_VERSION" "$OUR_VERSION" \
      "$(echo "$SR_LAST_OUTPUT" | tail -20)" \
      "$(echo "$OUR_LAST_OUTPUT" | tail -10)")
    report_result "$name" "FAIL" "$detail"
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

  run_sr "$dir"
  run_ours "$dir"
  compare "feat → minor (1.1.0)"
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

  run_sr "$dir"
  run_ours "$dir"
  compare "fix → patch (1.0.1)"
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

  run_sr "$dir"
  run_ours "$dir"
  compare "breaking change → major (2.0.0)"
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

  run_sr "$dir"
  run_ours "$dir"
  compare "BREAKING CHANGE footer → major (2.0.0)"
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

  run_sr "$dir"
  run_ours "$dir"

  # SR: NO_RELEASE. Us: returns current version (1.0.0) when no bump.
  if [ "$SR_VERSION" = "NO_RELEASE" ] && [ "$OUR_VERSION" = "1.0.0" ]; then
    report_result "chore → no release" "PASS" "sr=NO_RELEASE, us=1.0.0 [current]"
  else
    local detail
    detail=$(printf "semantic-release: %s\nsuper-release: %s\n%s" "$SR_VERSION" "$OUR_VERSION" "$(echo "$SR_LAST_OUTPUT" | tail -10)")
    report_result "chore → no release" "FAIL" "$detail"
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

  run_sr "$dir"
  run_ours "$dir"
  compare "mixed fix+feat → minor (1.1.0)"
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

  run_sr "$dir"
  run_ours "$dir"
  compare "perf → patch (1.0.1)"
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

  run_sr "$dir"
  run_ours "$dir"
  compare "first prerelease (1.1.0-beta.1)"
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

  run_sr "$dir"
  run_ours "$dir"
  compare "prerelease increment (1.1.0-beta.2)"
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

  run_sr "$dir"
  run_ours "$dir"
  compare "breaking on prerelease (2.0.0-beta.1)"
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

  run_sr "$dir"
  run_ours "$dir"
  compare "maintenance fix on 1.x (1.5.1)"
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

  run_sr "$dir"
  run_ours "$dir"
  compare "maintenance feat on 1.x (1.6.0)"
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

  local sr_output
  sr_output=$((cd "$dir" && "$SR_BIN" --no-ci) 2>&1) || true
  run_ours "$dir"

  # SR errors with EINVALIDNEXTVERSION. We cap to 1.6.0. Both prevent the bad release.
  local sr_errored=false
  if echo "$sr_output" | grep -q "EINVALIDNEXTVERSION"; then sr_errored=true; fi

  if [ "$sr_errored" = true ] && [ "$OUR_VERSION" = "1.6.0" ]; then
    report_result "maintenance breaking on 1.x" "PASS" "sr=EINVALIDNEXTVERSION, us=1.6.0 — both prevent bad release"
  else
    report_result "maintenance breaking on 1.x" "FAIL" "$(printf "sr_errored=%s, us=%s\n%s" "$sr_errored" "$OUR_VERSION" "$(echo "$sr_output" | tail -10)")"
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

  run_sr "$dir"
  run_ours "$dir"

  # Known difference: semantic-release always starts at 1.0.0.
  # super-release bumps from package.json version (0.0.0 → 0.1.0 for feat).
  # Both are valid — SR has a hardcoded initial version, we use package.json.
  if [ "$SR_VERSION" = "1.0.0" ] && [ "$OUR_VERSION" = "0.1.0" ]; then
    report_result "first release ever" "SKIP" "known difference: sr=$SR_VERSION, us=$OUR_VERSION — SR hardcodes 1.0.0, we use package.json"
  else
    compare "first release ever"
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

  run_sr "$dir"
  run_ours "$dir"
  compare "forward-port fix from 1.0.x to main (1.1.1)"
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

  git_push "$dir"
  local sr_output
  sr_output=$((cd "$dir" && "$SR_BIN" --no-ci) 2>&1) || true
  local our_output
  our_output=$("$SUPER_RELEASE" --dry-run -C "$dir" 2>&1) || true

  local sr_errored=false us_errored=false
  if echo "$sr_output" | grep -q "EINVALIDNEXTVERSION"; then sr_errored=true; fi
  if echo "$our_output" | grep -q "already exists as a tag"; then us_errored=true; fi

  if [ "$sr_errored" = true ] && [ "$us_errored" = true ]; then
    report_result "release branch collision (main vs next)" "PASS" "both error on version conflict"
  else
    report_result "release branch collision (main vs next)" "FAIL" \
      "$(printf "sr_errored=%s, us_errored=%s\n── sr ──\n%s\n── us ──\n%s" "$sr_errored" "$us_errored" "$(echo "$sr_output" | tail -10)" "$(echo "$our_output" | tail -10)")"
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

  local sr_output
  sr_output=$((cd "$dir" && "$SR_BIN" --no-ci) 2>&1) || true
  local our_output
  our_output=$("$SUPER_RELEASE" --dry-run -C "$dir" 2>&1) || true

  local sr_errored=false us_errored=false
  if echo "$sr_output" | grep -q "EINVALIDNEXTVERSION"; then sr_errored=true; fi
  if echo "$our_output" | grep -q "already exists as a tag"; then us_errored=true; fi

  if [ "$sr_errored" = true ] && [ "$us_errored" = true ]; then
    report_result "maintenance out-of-range feat on 1.x" "PASS" "both error on collision with v1.1.0"
  else
    report_result "maintenance out-of-range feat on 1.x" "FAIL" \
      "$(printf "sr_errored=%s, us_errored=%s\n── sr ──\n%s\n── us ──\n%s" "$sr_errored" "$us_errored" "$(echo "$sr_output" | tail -10)" "$(echo "$our_output" | tail -10)")"
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

  run_sr "$dir"
  run_ours "$dir"
  compare "prerelease 3rd increment (1.1.0-beta.3)"
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

  local tests=(
    test_feat_minor
    test_fix_patch
    test_breaking_major
    test_breaking_footer
    test_chore_no_release
    test_mixed_highest_wins
    test_perf_patch
    test_prerelease_first
    test_prerelease_increment
    test_prerelease_breaking
    test_prerelease_multiple_increments
    test_maintenance_fix
    test_maintenance_feat
    test_maintenance_breaking_capped
    test_forward_port_fix_from_maintenance
    test_release_branch_out_of_range
    test_maintenance_out_of_range_feat
    test_first_release
  )

  echo -e "${DIM}Running ${#tests[@]} tests (${PARALLEL} in parallel)...${NC}"
  echo ""

  # Track which results we've already printed
  local printed_count=0
  local total=${#tests[@]}

  # Print any new results that appeared since last check
  print_new_results() {
    local count=0
    for f in "$RESULTS_DIR"/*; do
      [ -f "$f" ] || continue
      count=$((count + 1))
    done

    if [ "$count" -gt "$printed_count" ]; then
      for f in "$RESULTS_DIR"/*; do
        [ -f "$f" ] || continue
        # Skip already printed (use a marker)
        [ -f "$f.printed" ] && continue
        touch "$f.printed"

        local status name detail
        status=$(sed -n '1p' "$f")
        name=$(sed -n '2p' "$f")
        detail=$(sed -n '3,$p' "$f")

        case "$status" in
          PASS)
            local version
            version=$(echo "$detail" | head -1)
            echo -e "  ${GREEN}PASS${NC} ${BOLD}$name${NC} ${DIM}($version)${NC}"
            ;;
          FAIL)
            echo -e "  ${RED}FAIL${NC} ${BOLD}$name${NC}"
            echo "$detail" | sed 's/^/       /'
            ;;
          SKIP)
            echo -e "  ${YELLOW}SKIP${NC} ${BOLD}$name${NC} ${DIM}($detail)${NC}"
            ;;
        esac
      done
      printed_count=$count
    fi
  }

  # Launch tests with bounded parallelism.
  local pids=()
  for test_fn in "${tests[@]}"; do
    "$test_fn" &
    pids+=($!)

    # Throttle: when we hit the limit, wait for one to finish
    if [ "${#pids[@]}" -ge "$PARALLEL" ]; then
      wait "${pids[0]}" 2>/dev/null || true
      pids=("${pids[@]:1}")
      print_new_results
    fi
  done

  # Wait for remaining tests
  for pid in "${pids[@]}"; do
    wait "$pid" 2>/dev/null || true
    print_new_results
  done

  # Final flush
  print_new_results

  # Count totals
  local pass=0 fail=0 skip=0
  for f in "$RESULTS_DIR"/*; do
    [ -f "$f" ] || continue
    [[ "$f" == *.printed ]] && continue
    case "$(head -1 "$f")" in
      PASS) pass=$((pass + 1)) ;;
      FAIL) fail=$((fail + 1)) ;;
      SKIP) skip=$((skip + 1)) ;;
    esac
  done

  echo ""
  echo "────────────────────────────────────────"
  echo -e "Results: ${GREEN}$pass passed${NC}, ${RED}$fail failed${NC}, ${YELLOW}$skip skipped${NC}"
  echo ""

  [ "$fail" -eq 0 ]
}

main "$@"
