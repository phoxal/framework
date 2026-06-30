#!/usr/bin/env bash
# Verify a coherent GHCR runtime image release.
#
# For a given framework version, confirm that EVERY runtime crate in the
# workspace has a published GHCR image whose tag resolves to a real
# multi-arch (linux/amd64 + linux/arm64) OCI index digest. This is the
# repeatable form of the "first coherent GHCR runtime image release" proof
# (phoxal/framework#31): the release matrix, the runtime crates, and the
# phoxal-cli platform-runtime catalog must all name the same set, and every
# image in that set must be pullable as a real digest pin.
#
# The runtime set is derived from the workspace (`runtime/<name>/Cargo.toml`),
# not a hard-coded list, so this gate fails if a runtime crate is added
# without a matching published image (or vice versa).
#
# Requires `docker buildx`. `imagetools inspect` queries the registry
# directly and does NOT need a running Docker daemon. For private packages,
# run `docker login ghcr.io` first.
#
# Usage:
#   scripts/verify-runtime-release.sh <VERSION>
#
# VERSION is required (e.g. 0.19.2). There is no shared workspace version to
# default to now that each crate is versioned independently (per-artifact
# release, plan #01).

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REGISTRY="${PHOXAL_RUNTIME_REGISTRY:-ghcr.io/phoxal}"

version="${1:-}"
[[ -n "${version}" ]] || { echo "usage: $(basename "$0") <VERSION> (e.g. 0.19.2)" >&2; exit 2; }

command -v docker >/dev/null 2>&1 || { echo "docker (with buildx) is required" >&2; exit 2; }

# Single source of truth: the runtime crates in the workspace. Each
# runtime/<name>/Cargo.toml builds phoxal-runtime-<name> and ships as
# ghcr.io/phoxal/runtime-<name>. The depth-1 glob excludes nested helper
# crates (e.g. runtime/localize/orb-slam3-sys) and the empty router/ dir.
# Images are API-version-scoped (api-version-availability): the tag is
# `<api>-v<version>` / `<api>-stable`, where <api> is the runtime's compiled-in
# API version, read here from its `#[phoxal(api = yYYYY_N)]` attribute.
runtimes=()
apis=()
for manifest in "${REPO_ROOT}"/runtime/*/Cargo.toml; do
  [[ -f "${manifest}" ]] || continue
  name="$(basename "$(dirname "${manifest}")")"
  main_rs="$(dirname "${manifest}")/src/main.rs"
  api="$(sed -n 's/.*#\[phoxal(.*api = \(y[0-9_]*\).*/\1/p' "${main_rs}" 2>/dev/null | head -n1)"
  [[ -n "${api}" ]] || { echo "could not read api version from ${main_rs}" >&2; exit 2; }
  runtimes+=("${name}")
  apis+=("${api}")
done
[[ ${#runtimes[@]} -gt 0 ]] || { echo "no runtime crates found under runtime/" >&2; exit 2; }

echo "Verifying ${#runtimes[@]} runtime images @ v${version} on ${REGISTRY}"
echo

fail=0
for i in "${!runtimes[@]}"; do
  r="${runtimes[$i]}"
  api="${apis[$i]}"
  ref="${REGISTRY}/runtime-${r}:${api}-v${version}"
  if ! raw="$(docker buildx imagetools inspect "${ref}" --raw 2>/dev/null)"; then
    printf '  %-12s MISSING    %s\n' "${r}" "${ref}"
    fail=$((fail + 1))
    continue
  fi
  digest="$(docker buildx imagetools inspect "${ref}" 2>/dev/null | awk '/^Digest:/{print $2; exit}')"
  has_amd=0; has_arm=0
  grep -q '"architecture": *"amd64"' <<<"${raw}" && has_amd=1
  grep -q '"architecture": *"arm64"' <<<"${raw}" && has_arm=1
  if [[ "${digest}" == sha256:* && ${has_amd} -eq 1 && ${has_arm} -eq 1 ]]; then
    printf '  %-12s OK  amd64+arm64  %s\n' "${r}" "${digest}"
  else
    printf '  %-12s FAIL  amd64=%s arm64=%s digest=%s\n' \
      "${r}" "${has_amd}" "${has_arm}" "${digest:-none}"
    fail=$((fail + 1))
  fi
done

echo
if [[ ${fail} -ne 0 ]]; then
  echo "FAIL: ${fail}/${#runtimes[@]} runtime images missing or not multi-arch @ ${version}" >&2
  exit 1
fi
echo "OK: all ${#runtimes[@]} runtime images published as multi-arch (amd64+arm64) indexes @ ${version}"
