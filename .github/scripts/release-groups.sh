#!/usr/bin/env bash
# Publishes the framework's unpublished packages to the phoxal registry
# in dependency-ready groups (see release_plan.py for the grouping).
#
# Restartable from observed state: a package whose current version is
# already live is complete and skipped, and its live archive bytes are
# still verified against the live index checksum. Newly submitted
# packages carry their exact submitted checksum through merge and
# deployment, and every package's live archive is downloaded and hashed
# before a group counts as done. A failure stops the run before any
# dependent group is submitted.
#
# Writes published-manifest.json recording this run's certified
# package/version/checksum set for downstream consumer verification.

set -euo pipefail

CLI="${CLI:-./target/debug/cargo-phoxal}"
INDEX="${REGISTRY_INDEX:-https://phoxal.github.io/registry}"
GH="${GH:-gh}"
MANIFEST_OUT="${MANIFEST_OUT:-published-manifest.json}"
VERIFY_TIMEOUT_POLLS="${VERIFY_TIMEOUT_POLLS:-40}"
POLL_INTERVAL="${POLL_INTERVAL:-15}"

log() { printf 'release-groups: %s\n' "$*" >&2; }

field() { # field <json> <key>
    printf '%s' "$1" | python3 -c "import sys,json; print(json.load(sys.stdin)['$2'])"
}

live_index_text() { # echoes the raw index; 22 = not deployed yet, 1 = failure
    local name="$1" p1 p2 code
    p1="${name:0:2}"; p2="${name:2:2}"
    code=0
    curl -fsS --max-time 30 "$INDEX/$p1/$p2/$name" 2>/dev/null || code=$?
    # 22 is an HTTP 404 (version not deployed yet); 37 is the
    # file:// equivalent used by the contract test's local index.
    if [ "$code" -eq 0 ] || [ "$code" -eq 22 ] || [ "$code" -eq 37 ]; then
        return "$code"
    fi
    log "index fetch failed for $name (curl exit $code)"
    return 1
}

live_entry_field() { # live_entry_field <name> <version> <field>
    local name="$1" version="$2" key="$3"
    live_index_text "$name" | python3 -c "
import sys, json
for line in sys.stdin:
    if not line.strip():
        continue
    entry = json.loads(line)
    if entry['vers'] == '$version':
        print(entry['$key'])
        break
else:
    sys.exit(3)
" || return $?
}

is_live_version() {
    [ "$(live_entry_field "$1" "$2" vers 2>/dev/null)" = "$2" ]
}

verify_live_archive() { # <name> <version> <expected sha256>
    local name="$1" version="$2" expected="$3" p1 p2 archive actual
    p1="${name:0:2}"; p2="${name:2:2}"
    archive="$RUNNER_TEMP/$name-$version.crate"
    if ! curl -fsS --max-time 60 "$INDEX/crates/$p1/$p2/$name/$version.crate" -o "$archive"; then
        log "live archive download failed for $name $version"
        return 1
    fi
    actual="$(sha256sum "$archive" | cut -d' ' -f1)"
    if [ "$actual" != "$expected" ]; then
        log "LIVE CHECKSUM MISMATCH for $name $version: $actual != $expected"
        return 1
    fi
    log "$name $version live bytes verified (sha256 $expected)"
}

wait_live() { # <name> <version> <expected sha256 or empty to trust the index cksum>
    local name="$1" version="$2" expected="${3:-}" poll
    for poll in $(seq 1 "$VERIFY_TIMEOUT_POLLS"); do
        local cksum
        if cksum="$(live_entry_field "$name" "$version" cksum)"; then
            if [ -z "$expected" ]; then
                expected="$cksum"  # resumed package: the trusted live record
            fi
            verify_live_archive "$name" "$version" "$expected"
            return 0
        elif [ $? -eq 3 ]; then
            : # version not deployed yet; keep polling
        else
            return 1  # network or parse failure already logged
        fi
        sleep "$POLL_INTERVAL"
    done
    log "$name $version did not appear in the live index within the timeout"
    return 1
}

wait_merged() {
    local url="$1"
    for _ in $(seq 1 60); do
        if "$GH" pr view "$url" --json state --jq .state | grep -qx MERGED; then
            return 0
        fi
        sleep "$POLL_INTERVAL"
    done
    log "submission $url did not merge within 15 minutes (admission or merge stalled)"
    return 1
}

# The planner command is overridable so the shell entry point itself can
# be exercised against a controlled plan with mocked endpoints.
PLAN="$(eval "${PLAN_CMD:-python3 .github/scripts/release_plan.py}")" || {
    log "the release planner failed"
    exit 1
}
PLAN_GROUPS="$(printf '%s' "$PLAN" | python3 -c "import sys,json; print(json.dumps(json.load(sys.stdin)['groups']))")" || {
    log "the release plan is not valid JSON"
    exit 1
}
GROUP_COUNT="$(printf '%s' "$PLAN_GROUPS" | python3 -c "import sys,json; print(len(json.load(sys.stdin)))")"
if [ "$GROUP_COUNT" -lt 1 ] || [ "$GROUP_COUNT" = "None" ]; then
    log "the release plan is unexpectedly empty; refusing to declare success"
    exit 1
fi
log "$GROUP_COUNT dependency-ready group(s)"

MANIFEST_TMP="$RUNNER_TEMP/manifest-lines"
SUBMITTED_SHAS="$RUNNER_TEMP/submitted-shas"
mkdir -p "$MANIFEST_TMP"
: > "$SUBMITTED_SHAS"

submitted_sha() { # <name> <version> -> echoes the sha submitted this run
    awk -v key="$1-$2" '$1 == key { print $2; exit }' "$SUBMITTED_SHAS"
}

for group_index in $(seq 0 $((GROUP_COUNT - 1))); do
    GROUP="$(printf '%s' "$PLAN_GROUPS" | python3 -c "import sys,json; print(json.dumps(json.load(sys.stdin)[$group_index]))")"
    ITEMS=()
    while IFS= read -r item; do
        [ -n "$item" ] && ITEMS+=("$item")
    done < <(printf '%s' "$GROUP" | python3 -c "import sys,json; [print(json.dumps(item)) for item in json.load(sys.stdin)]")
    if [ "${#ITEMS[@]}" -eq 0 ]; then
        log "failed to expand group $((group_index + 1))"
        exit 1
    fi
    log "group $((group_index + 1))/$GROUP_COUNT: $(printf '%s' "$GROUP" | python3 -c "import sys,json; print(' '.join(p['name'] for p in json.load(sys.stdin)))")"

    PR_URLS=()
    for ITEM in "${ITEMS[@]}"; do
        NAME="$(field "$ITEM" name)"
        VERSION="$(field "$ITEM" version)"
        KIND="$(field "$ITEM" kind)"
        DIR="$(field "$ITEM" dir)"
        if is_live_version "$NAME" "$VERSION"; then
            log "$NAME $VERSION is already live: complete, skipping submission"
            continue
        fi
        log "submitting $NAME $VERSION ($KIND)"
        OUTPUT="$("$CLI" publish "$KIND" "$NAME" --path "$DIR")"
        URL="$(printf '%s\n' "$OUTPUT" | grep -o 'https://github.com/phoxal/registry/pull/[0-9]*' | head -1)"
        SHA="$(printf '%s\n' "$OUTPUT" | grep -oE 'sha256: [0-9a-f]{64}' | head -1 | cut -d' ' -f2)"
        if [ -z "$URL" ] || [ -z "$SHA" ]; then
            log "submission produced no pull request or checksum for $NAME $VERSION"
            printf '%s\n' "$OUTPUT" >&2
            exit 1
        fi
        echo "$NAME-$VERSION $SHA" >> "$SUBMITTED_SHAS"
        echo "{\"name\": \"$NAME\", \"version\": \"$VERSION\", \"sha256\": \"$SHA\", \"pull_request\": \"$URL\"}" \
            >> "$MANIFEST_TMP/lines"
        log "submitted $NAME $VERSION: $URL sha256 $SHA (admission validates and merges)"
        PR_URLS+=("$URL")
    done

    for URL in "${PR_URLS[@]:-}"; do
        [ -n "$URL" ] && wait_merged "$URL"
    done
    # Every package in the group — submitted or resumed — must end this
    # group with verified live bytes before dependents may proceed.
    for ITEM in "${ITEMS[@]}"; do
        NAME="$(field "$ITEM" name)"
        VERSION="$(field "$ITEM" version)"
        wait_live "$NAME" "$VERSION" "$(submitted_sha "$NAME" "$VERSION")"
    done
done

python3 - "$MANIFEST_OUT" "$MANIFEST_TMP/lines" <<'PY'
import json
import sys
from pathlib import Path

target, lines_path = sys.argv[1], Path(sys.argv[2])
entries = [json.loads(line) for line in lines_path.read_text().splitlines() if line.strip()]
Path(target).write_text(json.dumps({"published": entries}, indent=1) + "\n")
print(f"release-groups: certified {len(entries)} submissions into {target}")
PY
log "all groups are live with verified bytes"
