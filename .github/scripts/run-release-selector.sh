#!/usr/bin/env bash
# The one caller of select-release-pr.sh.
#
# The release workflow runs this script, and the selector contract test
# runs the same file, so the caller side of the workflow boundary has
# exactly one source of truth: selector scratch lives under RUNNER_TEMP
# (outside the clean checkout), and the caller receives pr-url/pr-head by
# appending the selector output to GITHUB_OUTPUT.
#
# Environment (as the runner provides them):
#   RUNNER_TEMP    scratch directory outside the workspace checkout
#   GITHUB_OUTPUT  the step output file
# plus whatever select-release-pr.sh itself consumes. Must run from the
# repository root, like the workflow's run step.
set -euo pipefail
: "${RUNNER_TEMP:?the runner provides RUNNER_TEMP}"
: "${GITHUB_OUTPUT:?the runner provides GITHUB_OUTPUT}"
SELECTOR_OUT="$RUNNER_TEMP/selector-output" \
  bash .github/scripts/select-release-pr.sh
cat "$RUNNER_TEMP/selector-output" >> "$GITHUB_OUTPUT"
