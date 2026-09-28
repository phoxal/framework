#!/usr/bin/env bash
# Exercises the release-groups shell entry point itself with a tiny
# controlled plan and mocked publication, index, and merge endpoints.
# The registry index and archives are served from a local directory via
# file:// URLs, so no network or real publication happens. The mocks in
# test-mocks/ are driven by $SCENARIO state directories prepared here.
#
# Scenarios (all must hold for the suite to pass):
#  1. a controlled plan reaches submission, merges through the mock,
#     verifies live bytes, writes the manifest, and reports success;
#  2. a failing group stops the run before dependent work and never
#     prints the completed-release message;
#  3. an unexpectedly empty plan refuses to declare success;
#  4. a resumed package whose version is already live is verified
#     against live bytes — corrupted archive bytes fail the run.

set -euo pipefail

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ENTRY="$SCRIPT_DIR/release-groups.sh"
MOCKS="$SCRIPT_DIR/test-mocks"

pass=0
fail=0
check() { # check <description> <expected: ok|fail> <command...>
    local description="$1" expected="$2"
    shift 2
    if "$@" >"$WORK/out.log" 2>"$WORK/err.log"; then
        actual=ok
    else
        actual=fail
    fi
    if [ "$actual" = "$expected" ]; then
        printf 'PASS: %s\n' "$description"
        pass=$((pass + 1))
    else
        printf 'FAIL: %s (expected %s, got %s)\n--- stderr ---\n%s\n' \
            "$description" "$expected" "$actual" "$(tail -5 "$WORK/err.log")"
        fail=$((fail + 1))
    fi
}

new_scenario() { # <dir> <fail-after>
    local scenario="$1"
    mkdir -p "$scenario/bin" "$scenario/registry" "$scenario/state" "$scenario/tmp"
    echo "${2:-0}" > "$scenario/state/fail-after"
    cp "$MOCKS/cargo-phoxal-mock" "$scenario/bin/cli"
    cp "$MOCKS/gh-merge-mock" "$scenario/bin/gh"
    chmod +x "$scenario/bin/cli" "$scenario/bin/gh"
}

stage_package() { # <scenario> <name> <version> <bytes>
    local scenario="$1" name="$2" version="$3" bytes="$4"
    printf '%s' "$bytes" > "$scenario/state/$name.crate"
    sha256sum "$scenario/state/$name.crate" | cut -d' ' -f1 > "$scenario/state/$name.sha"
    echo "$version" > "$scenario/state/$name.ver"
}

run_entry() { # <scenario-dir> <plan-json>
    local scenario="$1" plan="$2"
    ( cd "$SCRIPT_DIR" && \
      SCENARIO="$scenario" \
      CLI="$scenario/bin/cli" \
      GH="$scenario/bin/gh" \
      INDEX="file://$scenario/registry" \
      REGISTRY_INDEX="file://$scenario/registry" \
      PLAN_CMD="printf '%s' '$plan'" \
      MANIFEST_OUT="$scenario/published-manifest.json" \
      RUNNER_TEMP="$scenario/tmp" \
      VERIFY_TIMEOUT_POLLS=6 \
      POLL_INTERVAL=1 \
      bash "$ENTRY" )
}

# ---------------------------------------------------------------- scenario 1
S="$WORK/happy"; new_scenario "$S" 0
stage_package "$S" pkg-alpha 0.1.0 'alpha-bytes'
stage_package "$S" pkg-beta 0.2.0 'beta-bytes'
PLAN='{"groups":[[{"name":"pkg-alpha","version":"0.1.0","kind":"library","dir":"a","deps":[]},{"name":"pkg-beta","version":"0.2.0","kind":"library","dir":"b","deps":[]}]]}'
check "happy path publishes, verifies live bytes, and writes the manifest" ok \
    run_entry "$S" "$PLAN"
if [ -f "$S/published-manifest.json" ] \
   && grep -q pkg-alpha "$S/published-manifest.json" \
   && grep -q "$(cat "$S/state/pkg-alpha.sha")" "$S/published-manifest.json"; then
    echo "PASS: manifest records the certified checksum"
    pass=$((pass + 1))
else
    echo "FAIL: manifest missing or incomplete"
    fail=$((fail + 1))
fi
grep -q "all groups are live with verified bytes" "$WORK/err.log" \
    && { echo "PASS: success message present"; pass=$((pass + 1)); } \
    || { echo "FAIL: success message missing"; fail=$((fail + 1)); }

# ---------------------------------------------------------------- scenario 2
S="$WORK/failing"; new_scenario "$S" 1
stage_package "$S" pkg-alpha 0.1.0 'alpha-bytes'
stage_package "$S" pkg-beta 0.2.0 'beta-bytes'
PLAN='{"groups":[[{"name":"pkg-alpha","version":"0.1.0","kind":"library","dir":"a","deps":[]},{"name":"pkg-beta","version":"0.2.0","kind":"library","dir":"b","deps":[]}]]}'
check "a failing group stops the run" fail run_entry "$S" "$PLAN"
if grep -q "all groups are live with verified bytes" "$WORK/err.log"; then
    echo "FAIL: completed-release message printed after failure"
    fail=$((fail + 1))
else
    echo "PASS: no success message after failure"
    pass=$((pass + 1))
fi
if [ -e "$S/published-manifest.json" ]; then
    echo "FAIL: manifest written despite failure"
    fail=$((fail + 1))
else
    echo "PASS: no manifest after failure"
    pass=$((pass + 1))
fi

# ---------------------------------------------------------------- scenario 3
S="$WORK/empty"; new_scenario "$S" 0
PLAN='{"groups":[]}'
check "an unexpectedly empty plan refuses success" fail run_entry "$S" "$PLAN"

# ---------------------------------------------------------------- scenario 4
S="$WORK/resumed"; new_scenario "$S" 0
stage_package "$S" pkg-alpha 0.1.0 'original-bytes'
# The version is already live, but the served archive is corrupted.
sha_a="$(cat "$S/state/pkg-alpha.sha")"
mkdir -p "$S/registry/pk/g-" "$S/registry/crates/pk/g-/pkg-alpha"
printf '{"name":"pkg-alpha","vers":"0.1.0","cksum":"%s","deps":[],"features":{},"yanked":false}\n' \
    "$sha_a" > "$S/registry/pk/g-/pkg-alpha"
printf 'corrupted-bytes' > "$S/registry/crates/pk/g-/pkg-alpha/0.1.0.crate"
PLAN='{"groups":[[{"name":"pkg-alpha","version":"0.1.0","kind":"library","dir":"a","deps":[]}]]}'
check "a resumed package with corrupted live bytes fails verification" fail \
    run_entry "$S" "$PLAN"
grep -q "LIVE CHECKSUM MISMATCH" "$WORK/err.log" \
    && { echo "PASS: mismatch reported"; pass=$((pass + 1)); } \
    || { echo "FAIL: mismatch not reported"; fail=$((fail + 1)); }

printf '\n%s\n' "release-groups contract: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
