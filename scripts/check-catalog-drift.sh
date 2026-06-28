#!/usr/bin/env bash
# Compare framework official runtime crates with phoxal-cli's compiled-in catalog.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CATALOG_RS="${1:-${PHOXAL_CLI_CATALOG_RS:-${ROOT}/../phoxal-cli/src/catalog.rs}}"

[[ -f "${CATALOG_RS}" ]] || {
  echo "catalog file not found: ${CATALOG_RS}" >&2
  echo "pass phoxal-cli/src/catalog.rs as the first argument or set PHOXAL_CLI_CATALOG_RS" >&2
  exit 2
}

framework_names="$(mktemp)"
catalog_names="$(mktemp)"
cleanup() {
  rm -f "${framework_names}" "${catalog_names}"
}
trap cleanup EXIT

for manifest in "${ROOT}"/runtime/*/Cargo.toml; do
  [[ -f "${manifest}" ]] || continue
  basename "$(dirname "${manifest}")"
done | sort > "${framework_names}"

sed -n 's/.*entry("\([^"]*\)".*/\1/p' "${CATALOG_RS}" | sort > "${catalog_names}"

if ! diff -u "${framework_names}" "${catalog_names}"; then
  echo "FAIL: framework runtime set and phoxal-cli PlatformRuntimeCatalog differ" >&2
  echo "framework: ${ROOT}/runtime/*" >&2
  echo "catalog:   ${CATALOG_RS}" >&2
  exit 1
fi

count="$(wc -l < "${framework_names}" | tr -d ' ')"
echo "OK: ${count} official runtimes match phoxal-cli catalog"
