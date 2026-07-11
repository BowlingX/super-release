#!/bin/sh
# Entry point for the super-release Docker container action: translates the
# action `inputs` (passed as INPUT_* env by action.yml) into CLI flags, then
# hands off to the binary. Also papers over the usual container-action git
# footguns.
set -eu

WS="${GITHUB_WORKSPACE:-/github/workspace}"

# The container runs as root while the repo is checked out by another UID;
# without this git refuses with "dubious ownership". Wildcard so arbitrary
# `docker run` mount points work too, not just $GITHUB_WORKSPACE.
git config --global --add safe.directory '*' 2>/dev/null || true

# libgit2 tag creation needs a committer identity; default to the Actions bot
# unless the workflow already configured one.
if ! git -C "$WS" config user.name >/dev/null 2>&1; then
  git config --global user.name "github-actions[bot]"
fi
if ! git -C "$WS" config user.email >/dev/null 2>&1; then
  git config --global user.email "41898282+github-actions[bot]@users.noreply.github.com"
fi

# Container actions don't receive the workflow token automatically.
if [ -n "${INPUT_GITHUB_TOKEN:-}" ]; then
  export GITHUB_TOKEN="$INPUT_GITHUB_TOKEN"
fi

# super-release needs full history + tags to compute versions.
if [ "$(git -C "$WS" rev-parse --is-shallow-repository 2>/dev/null || echo false)" = "true" ]; then
  echo "::warning::super-release needs full history and tags; check out with actions/checkout fetch-depth: 0"
fi

# Append input-derived flags to any args already passed (e.g. via `docker run`).
[ "${INPUT_DRY_RUN:-}" = "true" ] && set -- "$@" --dry-run
[ "${INPUT_PREVIEW:-}" = "true" ] && set -- "$@" --preview
[ "${INPUT_DANGEROUSLY_SKIP_CONFIG_CHECK:-}" = "true" ] && set -- "$@" --dangerously-skip-config-check
[ -n "${INPUT_CONFIG:-}" ] && set -- "$@" --config "$INPUT_CONFIG"
[ -n "${INPUT_PACKAGE:-}" ] && set -- "$@" --package "$INPUT_PACKAGE"
[ -n "${INPUT_WORKING_DIRECTORY:-}" ] && set -- "$@" -C "$INPUT_WORKING_DIRECTORY"
# Additional args
# shellcheck disable=SC2086
[ -n "${INPUT_ARGS:-}" ] && set -- "$@" $INPUT_ARGS

exec super-release "$@"
