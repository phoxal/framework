#!/usr/bin/env bash
# Compare framework official runtime crates with phoxal-cli's compiled-in catalog.
#
# Preferred input is the CLI's machine-readable catalog:
#   phoxal runtime catalog --message-format json
#
# TODO(cli-round-2): once that command is present in all supported CLI trains,
# remove the catalog.rs fallback below. The fallback derives base_image and
# participant_kind because the current source file only stores runtime ids.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CLI_BIN="${PHOXAL_CLI_BIN:-phoxal}"

default_catalog_source() {
  local dir="${ROOT}"
  while [[ "${dir}" != "/" ]]; do
    if [[ -f "${dir}/phoxal-cli/src/catalog.rs" ]]; then
      printf '%s\n' "${dir}/phoxal-cli/src/catalog.rs"
      return 0
    fi
    dir="$(dirname "${dir}")"
  done
  printf '%s\n' "${ROOT}/../phoxal-cli/src/catalog.rs"
}

CATALOG_SOURCE="${1:-${PHOXAL_CLI_CATALOG_JSON:-${PHOXAL_CLI_CATALOG_RS:-$(default_catalog_source)}}}"

framework_rows="$(mktemp)"
catalog_rows="$(mktemp)"
catalog_json="$(mktemp)"
cleanup() {
  rm -f "${framework_rows}" "${catalog_rows}" "${catalog_json}"
}
trap cleanup EXIT

for manifest in "${ROOT}"/runtime/*/Cargo.toml; do
  [[ -f "${manifest}" ]] || continue
  id="$(basename "$(dirname "${manifest}")")"
  printf '%s\t%s\t%s\n' "${id}" "ghcr.io/phoxal/runtime-${id}" "runtime"
done | sort > "${framework_rows}"

catalog_from_json() {
  local json_path="$1"
  python3 - "$json_path" > "${catalog_rows}" <<'PY'
import json
import sys

path = sys.argv[1]
with open(path, "r", encoding="utf-8") as fh:
    data = json.load(fh)

entries = data
if isinstance(data, dict):
    for key in ("entries", "runtimes", "catalog"):
        if key in data:
            entries = data[key]
            break

if not isinstance(entries, list):
    raise SystemExit("catalog JSON must be a list or an object with entries/runtimes/catalog")

rows = []
for entry in entries:
    if not isinstance(entry, dict):
        raise SystemExit("catalog entries must be JSON objects")
    runtime_id = entry.get("id") or entry.get("name")
    base_image = (
        entry.get("base_image")
        or entry.get("baseImage")
        or entry.get("image_repo")
        or entry.get("imageRepo")
    )
    participant_kind = entry.get("participant_kind") or entry.get("participantKind") or entry.get("kind")
    if not runtime_id or not base_image or not participant_kind:
        raise SystemExit(
            "catalog JSON entries must include id/name, base_image/image_repo, and participant_kind/kind"
        )
    rows.append((runtime_id, base_image, participant_kind))

for row in sorted(rows):
    print("\t".join(row))
PY
}

catalog_from_source_fallback() {
  local source_path="$1"
  [[ -f "${source_path}" ]] || {
    echo "catalog source not found and '${CLI_BIN} runtime catalog --message-format json' is unavailable: ${source_path}" >&2
    echo "set PHOXAL_CLI_BIN to a CLI with the JSON catalog command, pass a JSON dump, or set PHOXAL_CLI_CATALOG_RS" >&2
    exit 2
  }
  echo "WARN: using temporary catalog.rs parser fallback; waiting for CLI JSON catalog dump" >&2
  python3 - "$source_path" > "${catalog_rows}" <<'PY'
import re
import sys

path = sys.argv[1]
pattern = re.compile(r'\bentry\(\s*"([^"]+)"')
rows = []
with open(path, "r", encoding="utf-8") as fh:
    for line in fh:
        match = pattern.search(line)
        if match:
            runtime_id = match.group(1)
            rows.append((runtime_id, f"ghcr.io/phoxal/runtime-{runtime_id}", "runtime"))

for row in sorted(rows):
    print("\t".join(row))
PY
}

if [[ -f "${CATALOG_SOURCE}" && "${CATALOG_SOURCE}" == *.json ]]; then
  catalog_from_json "${CATALOG_SOURCE}"
elif command -v "${CLI_BIN}" >/dev/null 2>&1 \
  && "${CLI_BIN}" runtime catalog --message-format json > "${catalog_json}" 2>/dev/null; then
  catalog_from_json "${catalog_json}"
elif command -v phoxal-cli >/dev/null 2>&1 \
  && phoxal-cli runtime catalog --message-format json > "${catalog_json}" 2>/dev/null; then
  catalog_from_json "${catalog_json}"
else
  catalog_from_source_fallback "${CATALOG_SOURCE}"
fi

if ! diff -u "${framework_rows}" "${catalog_rows}"; then
  echo "FAIL: framework runtime catalog rows differ from phoxal-cli catalog" >&2
  echo "compared columns: id, base_image, participant_kind" >&2
  echo "framework: ${ROOT}/runtime/*" >&2
  echo "catalog:   ${CATALOG_SOURCE}" >&2
  exit 1
fi

count="$(wc -l < "${framework_rows}" | tr -d ' ')"
echo "OK: ${count} official runtime catalog rows match"
