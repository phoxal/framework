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
OUT="${SELECTOR_OUT:-selector-output}"
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
if ! release-plz release-pr 2>&1 | tee release-plz.log; then
    log "release-plz failed during version preparation; refusing to continue:"
    cat release-plz.log >&2
    exit 1
fi

url="$(grep -oE 'https://github.com/[^ ]+/pull/[0-9]+' release-plz.log | head -1 || true)"
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
{
    echo "pr-url=$selected_url"
    echo "pr-head=$selected_head"
} >> "$OUT"
log "selected $selected_url at head $selected_head"
