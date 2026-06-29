#!/usr/bin/env bash
# Compare framework official runtime crates with phoxal-cli's compiled-in catalog.
#
# Preferred input is the CLI's machine-readable catalog:
#   phoxal runtime catalog --message-format json
#
# The source fallback is intentionally fail-closed: it parses the current
# PlatformRuntimeCatalog shape, including image repo derivation and topology
# flags, and exits if the source no longer encodes those fields explicitly.

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

def first(entry, *names):
    for name in names:
        value = entry.get(name)
        if value is not None:
            return value
    return None

def bool_field(entry, *names):
    value = first(entry, *names)
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, str) and value in ("true", "false"):
        return value
    raise SystemExit(f"catalog JSON entries must include boolean {names[0]}")

rows = []
for entry in entries:
    if not isinstance(entry, dict):
        raise SystemExit("catalog entries must be JSON objects")
    runtime_id = first(entry, "id", "name")
    image_repo = first(entry, "image_repo", "imageRepo", "base_image", "baseImage")
    participant_kind = first(entry, "participant_kind", "participantKind", "kind")
    if not runtime_id or not image_repo or not participant_kind:
        raise SystemExit(
            "catalog JSON entries must include id/name, image_repo/base_image, and participant_kind/kind"
        )
    rows.append((
        runtime_id,
        image_repo,
        participant_kind,
        bool_field(entry, "uses_supervisor_api", "usesSupervisorApi"),
        bool_field(entry, "wires_to_router", "wiresToRouter"),
    ))

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
  echo "WARN: using structured catalog.rs parser fallback; prefer CLI JSON catalog dump" >&2
  python3 - "$source_path" > "${catalog_rows}" <<'PY'
import re
import sys

path = sys.argv[1]
with open(path, "r", encoding="utf-8") as fh:
    text = fh.read()

def fail(message):
    raise SystemExit(f"unsupported catalog.rs fallback shape: {message}")

def find_matching(source, open_index, open_char, close_char):
    depth = 0
    in_string = False
    escape = False
    for index in range(open_index, len(source)):
        char = source[index]
        if in_string:
            if escape:
                escape = False
            elif char == "\\":
                escape = True
            elif char == '"':
                in_string = False
            continue
        if char == '"':
            in_string = True
        elif char == open_char:
            depth += 1
        elif char == close_char:
            depth -= 1
            if depth == 0:
                return index
    fail(f"unmatched {open_char}")

def split_top_level(source):
    parts = []
    start = 0
    paren_depth = 0
    bracket_depth = 0
    in_string = False
    escape = False
    for index, char in enumerate(source):
        if in_string:
            if escape:
                escape = False
            elif char == "\\":
                escape = True
            elif char == '"':
                in_string = False
            continue
        if char == '"':
            in_string = True
        elif char == "(":
            paren_depth += 1
        elif char == ")":
            paren_depth -= 1
        elif char == "[":
            bracket_depth += 1
        elif char == "]":
            bracket_depth -= 1
        elif char == "," and paren_depth == 0 and bracket_depth == 0:
            parts.append(source[start:index].strip())
            start = index + 1
    tail = source[start:].strip()
    if tail:
        parts.append(tail)
    return parts

def parse_string(source, field):
    match = re.fullmatch(r'"([^"\\]*(?:\\.[^"\\]*)*)"', source.strip())
    if not match:
        fail(f"{field} must be a string literal")
    return bytes(match.group(1), "utf-8").decode("unicode_escape")

def parse_string_list(source):
    source = source.strip()
    if not source.startswith("&[") or not source.endswith("]"):
        fail("api_versions must be an explicit string-literal slice")
    inner = source[2:-1].strip()
    values = [parse_string(part, "api_version") for part in split_top_level(inner)]
    if not values:
        fail("api_versions must not be empty")
    return values

def parse_bool(source, field):
    source = source.strip()
    if source not in ("true", "false"):
        fail(f"{field} must be a bool literal")
    return source

if not re.search(r'\bpub\s+struct\s+PlatformRuntimeCatalog\b', text):
    fail("missing PlatformRuntimeCatalog type")
if not re.search(r'\bpub\s+struct\s+PlatformRuntimeEntry\b', text):
    fail("missing PlatformRuntimeEntry type")
if not re.search(
    r"\bconst\s+fn\s+entry\s*\(\s*name\s*:\s*&'static\s+str\s*,"
    r"\s*api_versions\s*:\s*&'static\s+\[\s*&'static\s+str\s*\]\s*,"
    r"\s*uses_supervisor_api\s*:\s*bool\s*,"
    r"\s*wires_to_router\s*:\s*bool\s*,",
    text,
):
    fail("entry helper signature no longer exposes name, api_versions, supervisor, and router fields")

image_fn = re.search(r'\bfn\s+image_repo\s*\([^)]*\)\s*->\s*String\s*\{(?P<body>.*?)\n\s*\}', text, re.S)
if not image_fn:
    fail("missing image_repo method")
image_format = re.search(r'format!\(\s*"([^"]*\{\}[^"]*)"\s*,\s*self\.name\s*\)', image_fn.group("body"))
if not image_format:
    fail("image_repo method must use a format string with self.name")
image_template = image_format.group(1)
if image_template.count("{}") != 1:
    fail("image_repo format string must contain exactly one placeholder")
image_prefix, image_suffix = image_template.split("{}")

catalog_match = re.search(
    r'\bpub\s+const\s+CATALOG\s*:\s*PlatformRuntimeCatalog\s*=\s*PlatformRuntimeCatalog\s*\{',
    text,
)
if not catalog_match:
    fail("missing CATALOG PlatformRuntimeCatalog constant")
entries_label = text.find("entries", catalog_match.end())
if entries_label == -1:
    fail("CATALOG has no entries field")
entries_open = text.find("[", entries_label)
if entries_open == -1:
    fail("CATALOG entries field is not a slice")
entries_close = find_matching(text, entries_open, "[", "]")
entries_source = text[entries_open + 1:entries_close]

rows = []
position = 0
while True:
    match = re.search(r'\bentry\s*\(', entries_source[position:])
    if not match:
        break
    call_start = position + match.start()
    paren_open = entries_source.find("(", call_start)
    paren_close = find_matching(entries_source, paren_open, "(", ")")
    args = split_top_level(entries_source[paren_open + 1:paren_close])
    if len(args) != 4:
        fail("entry calls must have exactly four arguments")
    runtime_id = parse_string(args[0], "runtime name")
    parse_string_list(args[1])
    uses_supervisor_api = parse_bool(args[2], "uses_supervisor_api")
    wires_to_router = parse_bool(args[3], "wires_to_router")
    rows.append((
        runtime_id,
        f"{image_prefix}{runtime_id}{image_suffix}",
        "runtime",
        uses_supervisor_api,
        wires_to_router,
    ))
    position = paren_close + 1

if not rows:
    fail("no catalog entries parsed")

for row in sorted(rows):
    print("\t".join(row))
PY
}

framework_from_source() {
  for manifest in "${ROOT}"/runtime/*/Cargo.toml; do
    [[ -f "${manifest}" ]] || continue
    id="$(basename "$(dirname "${manifest}")")"
    printf '%s\t%s\t%s\t%s\t%s\n' \
      "${id}" \
      "ghcr.io/phoxal/runtime-${id}" \
      "runtime" \
      "false" \
      "true"
  done | sort > "${framework_rows}"
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

framework_from_source

if ! diff -u "${framework_rows}" "${catalog_rows}"; then
  echo "FAIL: framework runtime catalog rows differ from phoxal-cli catalog" >&2
  echo "compared columns: id, image_repo, participant_kind, uses_supervisor_api, wires_to_router" >&2
  echo "framework: ${ROOT}/runtime/*" >&2
  echo "catalog:   ${CATALOG_SOURCE}" >&2
  exit 1
fi

count="$(wc -l < "${framework_rows}" | tr -d ' ')"
echo "OK: ${count} official runtime catalog rows match"
