#!/usr/bin/env bash
# Selects the one release version pull request this run may merge.
#
# A branch-name prefix alone proves nothing — anyone can open a pull
# request with a `release-plz-*` head from a fork or an unrelated
# account — so every candidate, whether taken from release-plz output
# or discovered among open pulls, must pass structured validation
# before this script reports it:
#
#   - lives in the expected repository and targets the expected base;
#   - open, and its head branch matches the release-plz version shape;
#   - head repository owner is the repository owner (no forks);
#   - authored by the trusted release App identity (bot login).
#
# Exactly one unambiguous candidate may be selected; the validated head
# SHA is emitted so the merge step binds to it. Failures — preparation
# errors, no eligible candidate, untrusted or forked candidates, or
# ambiguity — stop the run.
#
# Environment:
#   GITHUB_REPOSITORY        owner/name of the release repository
#   RELEASE_BASE             expected base branch (default: main)
#   TRUSTED_RELEASE_AUTHOR   the release App's bot login (default:
#                            app/phoxal-release-bot)
#   SELECTOR_OUT             file receiving "pr-url=…" / "pr-head=…"
#
# The release-plz and gh executables are resolved from PATH, so the
# contract test can drive this script with controlled fakes.

set -euo pipefail

GH="${GH:-gh}"
export RELEASE_BASE="${RELEASE_BASE:-main}"
export TRUSTED_RELEASE_AUTHOR="${TRUSTED_RELEASE_AUTHOR:-app/phoxal-release-bot}"
OUT="${SELECTOR_OUT:-${RUNNER_TEMP:-$(mktemp -d)}/selector-output}"
export SELECTOR_OWNER="${GITHUB_REPOSITORY%%/*}"
: > "$OUT"

log() { printf 'select-release-pr: %s\n' "$*" >&2; }

VALIDATE_PY='
import json, os, sys
pull = json.load(sys.stdin)
owner = os.environ["SELECTOR_OWNER"]
base = os.environ["RELEASE_BASE"]
trusted = os.environ["TRUSTED_RELEASE_AUTHOR"]
author = pull.get("author") or {}
problems = []
if pull.get("state") != "OPEN":
    problems.append("state is %r" % pull.get("state"))
if pull.get("baseRefName") != base:
    problems.append("base is %r" % pull.get("baseRefName"))
head = pull.get("headRefName") or ""
if not head.startswith("release-plz-"):
    problems.append("head branch %r is not a release-plz version branch" % head)
head_owner = (pull.get("headRepositoryOwner") or {}).get("login")
if head_owner != owner:
    problems.append("head repository owner is %r, not %r" % (head_owner, owner))
if pull.get("isCrossRepository"):
    problems.append("head is cross-repository (fork)")
if author.get("login") != trusted or not author.get("is_bot"):
    problems.append(
        "author is %r (bot=%r), not the trusted release App %r"
        % (author.get("login"), author.get("is_bot"), trusted)
    )
if problems:
    print("; ".join(problems), file=sys.stderr)
    sys.exit(1)
print(pull["url"], pull["headRefOid"])
'

FILTER_PY='
import json, os, sys
owner = os.environ["SELECTOR_OWNER"]
base = os.environ["RELEASE_BASE"]
trusted = os.environ["TRUSTED_RELEASE_AUTHOR"]
pulls = json.load(sys.stdin)
if isinstance(pulls, dict):
    pulls = [pulls]
eligible = []
for pull in pulls:
    author = pull.get("author") or {}
    if (pull.get("state") == "OPEN"
            and pull.get("baseRefName") == base
            and (pull.get("headRefName") or "").startswith("release-plz-")
            and (pull.get("headRepositoryOwner") or {}).get("login") == owner
            and not pull.get("isCrossRepository")
            and author.get("login") == trusted
            and author.get("is_bot")):
        eligible.append(str(pull["number"]))
print(" ".join(eligible))
'

validate_pull() { # <pull number> -> echoes "<url> <sha>" or fails
    local number="$1"
    "$GH" pr view "$number" \
        --json url,state,baseRefName,headRefName,headRepositoryOwner,isCrossRepository,author,headRefOid \
        | python3 -c "$VALIDATE_PY"
}

# Version preparation failures stop the run; they are never treated as
# "nothing to release".
# Scratch files live outside the checkout: release-plz requires a
# clean working tree, and its own log must not dirty it.
LOG="${RELEASE_PLZ_LOG:-${RUNNER_TEMP:-$(mktemp -d)}/release-plz.log}"
if ! release-plz release-pr 2>&1 | tee "$LOG"; then
    log "release-plz failed during version preparation; refusing to continue:"
    cat "$LOG" >&2
    exit 1
fi

# The SDK asserts that phoxal and phoxal-build carry identical versions
# (the generated-code marker hashes the crate version), but release-plz
# plans independent per-package releases: a release whose commits
# changed only phoxal leaves the helper behind and every generated
# consumer fails the marker assert. Re-align the helper with the SDK on
# the version branch before anything is selected or merged. When an
# alignment commit is pushed, its exact head is recorded so selection
# binds to the pushed sha — the pull API can briefly keep serving the
# previous head oid. No-op without the helper manifest (the
# contract-test scenarios), without an open release-plz PR, or when the
# pair already agrees.
ALIGN_HEAD_FILE="${RUNNER_TEMP:-$(mktemp -d)}/release-alignment-head"
align_build_helper_version() {
    [ -f phoxal/build/Cargo.toml ] || return 0
    local original_sha original_ref branch number sdk helper
    original_sha="$(git rev-parse HEAD)"
    original_ref="$(git rev-parse --abbrev-ref HEAD)"
    read -r number branch < <("$GH" pr list --state open --json number,headRefName | python3 -c '
import json, sys
pulls = json.load(sys.stdin)
match = next((pull for pull in pulls if (pull["headRefName"] or "").startswith("release-plz-")), None)
print(str(match["number"]) + " " + match["headRefName"] if match else "")
')
    [ -n "$number" ] && [ -n "$branch" ] || return 0
    git fetch -q origin "$branch"
    git checkout -q -B release-alignment "origin/$branch"
    sdk="$(perl -ne 'if (!$d && /^version = "(.*)"/) { print $1; $d = 1 }' phoxal/Cargo.toml)"
    helper="$(perl -ne 'if (!$d && /^version = "(.*)"/) { print $1; $d = 1 }' phoxal/build/Cargo.toml)"
    if [ "$sdk" != "$helper" ]; then
        log "aligning phoxal-build $helper with the SDK $sdk on $branch"
        perl -pi -e 'BEGIN { $done = 0 }
            if (!$done && s/^version = ".*"$/version = "'"$sdk"'"/) { $done = 1 }' \
            phoxal/build/Cargo.toml
        perl -pi -e 's{^(phoxal-build = \{ path = "phoxal/build", version = ")=[^"]*(")}{${1}='"$sdk"'${2}}' \
            Cargo.toml
        cargo update -p phoxal-build --quiet
        git add Cargo.toml phoxal/build/Cargo.toml
        [ -f Cargo.lock ] && git add Cargo.lock
        git -c user.name=phoxal-release-bot \
            -c user.email=release-bot@phoxal.invalid \
            commit -qm "Align phoxal-build with the SDK version $sdk"
        git push -q origin "HEAD:$branch"
        printf '%s %s\n' "$number" "$(git rev-parse HEAD)" > "$ALIGN_HEAD_FILE"
    fi
    if [ "$original_ref" != "HEAD" ]; then
        git checkout -q "$original_ref"
    else
        git checkout -q "$original_sha"
    fi
}
align_build_helper_version

url="$(grep -oE 'https://github.com/[^ ]+/pull/[0-9]+' "$LOG" | head -1 || true)"
verdict=""

if [ -n "$url" ]; then
    # The URL release-plz printed still gets fully validated; a forged
    # or moved pull never reaches the merge step.
    number="${url##*/}"
    if ! verdict="$(validate_pull "$number")"; then
        log "release-plz reported $url, but it failed validation; refusing to merge it"
        exit 1
    fi
else
    # Nothing new to release: discover open candidates and filter them
    # with the same structured criteria.
    listing="$("$GH" pr list --state open \
        --json number,url,state,baseRefName,headRefName,headRepositoryOwner,isCrossRepository,author)"
    candidates="$(printf '%s' "$listing" | python3 -c "$FILTER_PY")"
    count="$(wc -w <<< "$candidates")"
    if [ "$count" -eq 0 ]; then
        log "no version PR was created and no eligible release-plz PR is open; nothing to release"
        exit 1
    fi
    if [ "$count" -gt 1 ]; then
        log "ambiguous release selection: open eligible version PRs $candidates; refusing to pick one blindly"
        exit 1
    fi
    number="${candidates// /}"
    if ! verdict="$(validate_pull "$number")"; then
        log "the discovered candidate #$number failed validation"
        exit 1
    fi
    log "resuming the open release-plz version PR #$number"
fi

read -r selected_url selected_head <<< "$verdict"
# When the alignment pushed a new head for this very pull, bind to the
# pushed sha: the pull API can briefly keep serving the previous head
# oid after the push.
if [ -f "$ALIGN_HEAD_FILE" ]; then
    read -r aligned_number aligned_head < "$ALIGN_HEAD_FILE"
    if [ "$aligned_number" = "${selected_url##*/}" ]; then
        selected_head="$aligned_head"
        log "binding to the alignment head $selected_head"
    fi
fi
{
    echo "pr-url=$selected_url"
    echo "pr-head=$selected_head"
} >> "$OUT"
log "selected $selected_url at head $selected_head"
