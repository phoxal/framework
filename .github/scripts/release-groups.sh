#!/usr/bin/env bash
# Publishes the framework's unpublished packages to the phoxal registry
# in dependency-ready groups (see release_plan.py for the grouping).
#
# Restartable from observed state: a package whose current version is
# already live is complete and skipped; every submission goes through
# registry admission (integrity + ownership + source report), which
# auto-merges eligible PRs; publication advances only after the live
# index serves the version with the checksum recorded in its index line.
# A failure stops the run before any dependent group is submitted.

set -euo pipefail

CLI="${CLI:-./target/debug/cargo-phoxal}"
INDEX="${REGISTRY_INDEX:-https://phoxal.github.io/registry}"
GH="${GH:-gh}"

log() { printf 'release-groups: %s\n' "$*" >&2; }

field() { # field <json> <key>
    printf '%s' "$1" | python3 -c "import sys,json; print(json.load(sys.stdin)['$2'])"
}

live_versions() {
    local name="$1" p1 p2
    p1="${name:0:2}"; p2="${name:2:2}"
    curl -fsSL "$INDEX/$p1/$p2/$name" 2>/dev/null \
        | python3 -c "import sys,json; [print(json.loads(l)['vers']) for l in sys.stdin if l.strip()]" \
        || true
}

merged_index_checksum() { # from the merged registry main
    local name="$1" version="$2" p1 p2
    p1="${name:0:2}"; p2="${name:2:2}"
    "$GH" api "repos/phoxal/registry/contents/$p1/$p2/$name" --jq .content \
        | base64 -d | python3 -c "
import sys, json
for line in sys.stdin:
    if not line.strip():
        continue
    entry = json.loads(line)
    if entry['vers'] == '$version':
        print(entry['cksum'])
        break
"
}

wait_live() {
    local name="$1" version="$2" expected="$3" p1 p2 archive actual
    p1="${name:0:2}"; p2="${name:2:2}"
    for _ in $(seq 1 40); do
        if [ "$(live_versions "$name" | grep -cxF "$version")" -ge 1 ]; then
            archive="$RUNNER_TEMP/$name-$version.crate"
            curl -fsSL "$INDEX/crates/$p1/$p2/$name/$version.crate" -o "$archive"
            actual="$(sha256sum "$archive" | cut -d' ' -f1)"
            if [ "$actual" = "$expected" ]; then
                log "$name $version is live with the expected checksum"
                return 0
            fi
            log "LIVE CHECKSUM MISMATCH for $name $version: $actual != $expected"
            return 1
        fi
        sleep 15
    done
    log "$name $version did not appear in the live index within 10 minutes"
    return 1
}

wait_merged() {
    local url="$1"
    for _ in $(seq 1 60); do
        if "$GH" pr view "$url" --json state --jq .state | grep -qx MERGED; then
            return 0
        fi
        sleep 15
    done
    log "submission $url did not merge within 15 minutes (admission or auto-merge stalled)"
    return 1
}

PLAN="$(python3 .github/scripts/release_plan.py)"
GROUPS="$(printf '%s' "$PLAN" | python3 -c "import sys,json; print(json.dumps(json.load(sys.stdin)['groups']))")"
GROUP_COUNT="$(printf '%s' "$GROUPS" | python3 -c "import sys,json; print(len(json.load(sys.stdin)))")"
log "$GROUP_COUNT dependency-ready group(s)"

for group_index in $(seq 0 $((GROUP_COUNT - 1))); do
    GROUP="$(printf '%s' "$GROUPS" | python3 -c "import sys,json; print(json.dumps(json.load(sys.stdin)[$group_index]))")"
    mapfile -t ITEMS < <(printf '%s' "$GROUP" | python3 -c "import sys,json; [print(json.dumps(item)) for item in json.load(sys.stdin)]")
    log "group $((group_index + 1))/$GROUP_COUNT: $(printf '%s' "$GROUP" | python3 -c "import sys,json; print(' '.join(p['name'] for p in json.load(sys.stdin)))")"

    PR_URLS=()
    for ITEM in "${ITEMS[@]}"; do
        NAME="$(field "$ITEM" name)"
        VERSION="$(field "$ITEM" version)"
        KIND="$(field "$ITEM" kind)"
        DIR="$(field "$ITEM" dir)"
        if [ "$(live_versions "$NAME" | grep -cxF "$VERSION")" -ge 1 ]; then
            log "$NAME $VERSION is already live: complete, skipping submission"
            continue
        fi
        log "submitting $NAME $VERSION ($KIND)"
        OUTPUT="$("$CLI" publish "$KIND" "$NAME" --path "$DIR")"
        URL="$(printf '%s\n' "$OUTPUT" | grep -o 'https://github.com/phoxal/registry/pull/[0-9]*' | head -1)"
        if [ -z "$URL" ]; then
            log "submission produced no pull request for $NAME $VERSION"
            printf '%s\n' "$OUTPUT" >&2
            exit 1
        fi
        log "submitted $NAME $VERSION: $URL (admission validates and auto-merges)"
        PR_URLS+=("$URL")
    done

    for URL in "${PR_URLS[@]:-}"; do
        [ -n "$URL" ] && wait_merged "$URL"
    done
    for ITEM in "${ITEMS[@]}"; do
        NAME="$(field "$ITEM" name)"
        VERSION="$(field "$ITEM" version)"
        if [ "$(live_versions "$NAME" | grep -cxF "$VERSION")" -ge 1 ]; then
            continue
        fi
        wait_live "$NAME" "$VERSION" "$(merged_index_checksum "$NAME" "$VERSION")"
    done
done

log "all groups are live and verified"
