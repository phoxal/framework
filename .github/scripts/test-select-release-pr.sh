#!/usr/bin/env bash
# Exercises the release version-PR selector with mocked release-plz
# and gh executables. No remote state is touched and no release is
# minted: every scenario is a controlled fake in a scenario directory.
#
# Scenarios (all must hold for the suite to pass):
#  1. a failed release-plz preparation stops the run;
#  2. success without output and no open PR reports nothing to release;
#  3. an untrusted author or a forked/cross-repository release-plz-* PR
#     is never selected — via discovery and via a printed URL;
#  4. two eligible candidates are an ambiguity error;
#  5. a genuine App-authored same-repository version PR is selected
#     with its head SHA.

set -euo pipefail

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SELECTOR="$SCRIPT_DIR/select-release-pr.sh"

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
            "$description" "$expected" "$actual" "$(tail -4 "$WORK/err.log")"
        fail=$((fail + 1))
    fi
}

# Writes one open-pull record shaped like `gh pr view/list --json`.
pull_json() { # <number> <head> <author> <is_bot> <owner> <cross> [state] [base]
    python3 - "$@" <<'PY'
import json, sys
number, head, author, is_bot, owner, cross = sys.argv[1:7]
state = sys.argv[7] if len(sys.argv) > 7 and sys.argv[7] else "OPEN"
base = sys.argv[8] if len(sys.argv) > 8 and sys.argv[8] else "main"
print(json.dumps({
    "number": int(number),
    "url": f"https://github.com/phoxal/framework/pull/{number}",
    "state": state,
    "baseRefName": base,
    "headRefName": head,
    "headRepositoryOwner": {"login": owner},
    "isCrossRepository": cross == "true",
    "author": {"login": author, "is_bot": is_bot == "true"},
    "headRefOid": f"sha{number}",
}))
PY
}

# Builds a scenario with faked release-plz and gh on PATH. The fakes
# are driven by files in $SCENARIO/state:
#   prep-fails     release-plz exits 1 with a message
#   prep-output    text release-plz prints (default: nothing to release)
#   pulls.json     array of pull records for `gh pr list`
make_scenario() { # <dir>
    local scenario="$1"
    mkdir -p "$scenario/bin" "$scenario/state"
    echo 0 > "$scenario/state/prep-fails"
    echo "Nothing to release." > "$scenario/state/prep-output"
    echo "[]" > "$scenario/state/pulls.json"
    cat > "$scenario/bin/release-plz" <<EOF
#!/usr/bin/env bash
set -euo pipefail
if [ "\$(cat "$scenario/state/prep-fails")" = "1" ]; then
  echo "release-plz panicked during preparation" >&2
  exit 1
fi
cat "$scenario/state/prep-output"
EOF
    cat > "$scenario/bin/gh" <<EOF
#!/usr/bin/env bash
set -euo pipefail
# gh pr view <number> --json ...
if [ "\$1" = "pr" ] && [ "\$2" = "view" ]; then
  number="\$3"
  python3 -c '
import json, sys
pulls = json.load(open(sys.argv[1]))
if isinstance(pulls, dict):
    pulls = [pulls]
match = next(p for p in pulls if str(p["number"]) == sys.argv[2])
print(json.dumps(match))
' "$scenario/state/pulls.json" "\$number"
  exit 0
fi
# gh pr list --state open --json ...
if [ "\$1" = "pr" ] && [ "\$2" = "list" ]; then
  cat "$scenario/state/pulls.json"
  exit 0
fi
echo "unexpected gh invocation: \$*" >&2
exit 1
EOF
    chmod +x "$scenario/bin/release-plz" "$scenario/bin/gh"
}

run_selector() { # <scenario-dir>
    local scenario="$1"
    ( cd "$scenario" && \
      PATH="$scenario/bin:$PATH" \
      GITHUB_REPOSITORY="phoxal/framework" \
      SELECTOR_OUT="$scenario/selector-output" \
      bash "$SELECTOR" )
}

# ---------------------------------------------------------------- scenario 1
S="$WORK/prep-fails"; make_scenario "$S"; echo 1 > "$S/state/prep-fails"
check "failed version preparation stops the run" fail run_selector "$S"

# ---------------------------------------------------------------- scenario 2
S="$WORK/no-candidate"; make_scenario "$S"
check "no eligible open PR reports nothing to release" fail run_selector "$S"
grep -q "no eligible release-plz PR is open" "$WORK/err.log" \
    && { echo "PASS: explicit no-candidate message"; pass=$((pass + 1)); } \
    || { echo "FAIL: no explicit no-candidate message"; fail=$((fail + 1)); }

# ---------------------------------------------------------------- scenario 3
S="$WORK/untrusted"; make_scenario "$S"
pull_json 777 release-plz-untrusted mallory false phoxal false > "$WORK/p1.json"
pull_json 778 release-plz-untrusted app/phoxal-release-bot true someone-else true > "$WORK/p2.json"
python3 -c "import json,sys; print(json.dumps([json.load(open(sys.argv[1])), json.load(open(sys.argv[2]))]))" "$WORK/p1.json" "$WORK/p2.json" > "$S/state/pulls.json"
check "untrusted-author and forked release-plz-* PRs are never selected" fail \
    run_selector "$S"
grep -q "no eligible" "$WORK/err.log" \
    && { echo "PASS: untrusted candidates excluded by the filter"; pass=$((pass + 1)); } \
    || { echo "FAIL: untrusted candidates not excluded"; fail=$((fail + 1)); }

# A printed URL to an untrusted pull is refused just the same.
S="$WORK/untrusted-url"; make_scenario "$S"
cp "$WORK/p1.json" "$S/state/pulls.json"
echo "Opened PR at https://github.com/phoxal/framework/pull/777" > "$S/state/prep-output"
check "a printed URL to an untrusted pull is refused" fail run_selector "$S"
grep -q "failed validation" "$WORK/err.log" \
    && { echo "PASS: untrusted URL rejected in validation"; pass=$((pass + 1)); } \
    || { echo "FAIL: untrusted URL not rejected"; fail=$((fail + 1)); }

# ---------------------------------------------------------------- scenario 4
S="$WORK/ambiguous"; make_scenario "$S"
pull_json 801 release-plz-a app/phoxal-release-bot true phoxal false > "$WORK/a1.json"
pull_json 802 release-plz-b app/phoxal-release-bot true phoxal false > "$WORK/a2.json"
python3 -c "import json,sys; print(json.dumps([json.load(open(sys.argv[1])), json.load(open(sys.argv[2]))]))" "$WORK/a1.json" "$WORK/a2.json" > "$S/state/pulls.json"
check "two eligible candidates are an ambiguity error" fail run_selector "$S"
grep -q "ambiguous" "$WORK/err.log" \
    && { echo "PASS: ambiguity reported"; pass=$((pass + 1)); } \
    || { echo "FAIL: ambiguity not reported"; fail=$((fail + 1)); }

# ---------------------------------------------------------------- scenario 5
S="$WORK/genuine"; make_scenario "$S"
pull_json 495 release-plz-2026-09-28T20-12-17Z app/phoxal-release-bot true phoxal false > $WORK/g495.json
python3 -c "import json,sys; print(json.dumps([json.load(open(sys.argv[1]))]))" "$WORK/g495.json" > "$S/state/pulls.json"
check "a genuine App version PR is selected" ok run_selector "$S"
if grep -q "pr-url=https://github.com/phoxal/framework/pull/495" "$S/selector-output" \
   && grep -q "pr-head=sha495" "$S/selector-output"; then
    echo "PASS: selection records the validated URL and head SHA"
    pass=$((pass + 1))
else
    echo "FAIL: selection output incomplete: $(cat "$S/selector-output")"
    fail=$((fail + 1))
fi

# The genuine shape also flows through release-plz's printed URL.
S="$WORK/genuine-url"; make_scenario "$S"
cp "$WORK/genuine/state/pulls.json" "$S/state/pulls.json"
echo "here: https://github.com/phoxal/framework/pull/495" > "$S/state/prep-output"
check "a genuine printed URL is selected with its head SHA" ok run_selector "$S"
grep -q "pr-head=sha495" "$S/selector-output" \
    && { echo "PASS: URL path records the head SHA"; pass=$((pass + 1)); } \
    || { echo "FAIL: URL path missing head SHA"; fail=$((fail + 1)); }


# ---------------------------------------------------------------- alignment
# The SDK asserts phoxal and phoxal-build carry identical versions; a
# version branch that bumped only the SDK must gain an alignment commit
# on the helper (crate manifest and workspace pin) before selection,
# with the worktree restored afterwards.
S="$WORK/align"; make_scenario "$S"
pull_json 496 release-plz-align-x app/phoxal-release-bot true phoxal false > "$WORK/a496.json"
python3 -c "import json,sys; print(json.dumps([json.load(open(sys.argv[1]))]))" "$WORK/a496.json" > "$S/state/pulls.json"
# cargo is not exercised beyond lock refresh; a no-op fake keeps the
# scenario hermetic.
printf '#!/usr/bin/env bash\nexit 0\n' > "$S/bin/cargo"
chmod +x "$S/bin/cargo"
mkdir -p "$S/phoxal/build"
printf '[package]\nname = "phoxal"\nversion = "0.0.0-dev.6"\n' > "$S/phoxal/Cargo.toml"
printf '[package]\nname = "phoxal-build"\nversion = "0.0.0-dev.6"\n' > "$S/phoxal/build/Cargo.toml"
printf '[workspace.dependencies]\nphoxal-build = { path = "phoxal/build", version = "=0.0.0-dev.6", registry = "phoxal" }\n' > "$S/Cargo.toml"
git -C "$S" init -q -b main
git -C "$S" add -A
git -C "$S" -c user.name=t -c user.email=t@t commit -qm base
git -C "$S" checkout -q -b release-plz-align-x
perl -pi -e 's/^version = ".*"$/version = "0.0.0-dev.7"/' "$S/phoxal/Cargo.toml"
git -C "$S" add -A
git -C "$S" -c user.name=t -c user.email=t@t commit -qm "chore(release): update package versions"
git -C "$S" checkout -q main
git -C "$S" remote add origin "$S"
check "a drifted helper version still selects" ok run_selector "$S"
alignment="$(git -C "$S" log -1 --format=%s "origin/release-plz-align-x")"
helper_version="$(git -C "$S" show "origin/release-plz-align-x:phoxal/build/Cargo.toml" | perl -ne 'if (/^version = "(.+)"/) { print $1; last }')"
pinned="$(git -C "$S" show "origin/release-plz-align-x:Cargo.toml" | perl -ne 'if (/phoxal-build = \{ path = "phoxal\/build", version = "=([^"]+)"/) { print $1; last }')"
if [ "$alignment" = "Align phoxal-build with the SDK version 0.0.0-dev.7" ] \
   && [ "$helper_version" = "0.0.0-dev.7" ] \
   && [ "$pinned" = "0.0.0-dev.7" ]; then
    echo "PASS: the helper version and workspace pin were aligned on the branch"
    pass=$((pass + 1))
else
    echo "FAIL: alignment commit: $alignment; helper: $helper_version; pin: $pinned"
    fail=$((fail + 1))
fi
if [ "$(git -C "$S" branch --show-current)" = "main" ] \
   && [ -z "$(git -C "$S" status --porcelain -uno)" ]; then
    echo "PASS: the alignment restores the original checkout"
    pass=$((pass + 1))
else
    echo "FAIL: worktree not restored after alignment"
    fail=$((fail + 1))
fi

# ---------------------------------------------------------------- boundary
# The real workflow caller boundary: a clean git checkout containing the
# workflow's own caller script, RUNNER_TEMP and GITHUB_OUTPUT as the
# runner provides them, the same `run:` command release.yml uses, and the
# assertions that the checkout stays clean and the caller receives the
# output. The caller commands are executed, never re-typed here.
S="$WORK/boundary"; make_scenario "$S"
pull_json 495 release-plz-2026-09-29T05-00-00Z app/phoxal-release-bot true phoxal false > "$WORK/b495.json"
python3 -c "import json,sys; print(json.dumps([json.load(open(sys.argv[1]))]))" "$WORK/b495.json" > "$S/state/pulls.json"
mkdir -p "$S/.github/scripts"
cp "$SCRIPT_DIR/select-release-pr.sh" "$SCRIPT_DIR/run-release-selector.sh" "$S/.github/scripts/"
git -C "$S" init -q
git -C "$S" add -A
git -C "$S" -c user.name=t -c user.email=t@t commit -qm base
runner_temp="$WORK/boundary-runner"; mkdir -p "$runner_temp"
# The runner provides GITHUB_OUTPUT outside the workspace; pointing it
# into the checkout would itself dirty the tree.
GITHUB_OUTPUT="$runner_temp/github-output" \
PATH="$S/bin:$PATH" GITHUB_REPOSITORY="phoxal/framework" \
RUNNER_TEMP="$runner_temp" bash -c \
    'cd "$1" && bash .github/scripts/run-release-selector.sh' \
    _ "$S" > "$WORK/boundary-out.log" 2>&1
boundary_rc=$?
if [ "$boundary_rc" -eq 0 ]; then
    echo "PASS: the caller boundary selects in a clean checkout"
    pass=$((pass + 1))
else
    echo "FAIL: the caller boundary failed"
    tail -3 "$WORK/boundary-out.log" >&2
    fail=$((fail + 1))
fi
if grep -q "^pr-url=https://github.com/phoxal/framework/pull/495$" "$runner_temp/github-output" \
   && grep -q "^pr-head=sha495$" "$runner_temp/github-output"; then
    echo "PASS: the caller receives pr-url and pr-head"
    pass=$((pass + 1))
else
    echo "FAIL: caller output incomplete: $(cat "$runner_temp/github-output" 2>/dev/null)"
    fail=$((fail + 1))
fi
if [ -z "$(git -C "$S" status --porcelain)" ]; then
    echo "PASS: the checkout stays clean"
    pass=$((pass + 1))
else
    echo "FAIL: selector dirtied the checkout: $(git -C "$S" status --porcelain)"
    fail=$((fail + 1))
fi

printf '\n%s\n' "select-release-pr contract: $pass passed, $fail failed"
[ "$fail" -eq 0 ]
