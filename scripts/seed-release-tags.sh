#!/usr/bin/env bash
# One-time bootstrap of per-package release tags for plan #01 (per-artifact
# release).
#
# Before plan #01 the workspace released under a single shared tag (`v<version>`).
# release-plz now tracks each crate by its OWN tag `<crate>-v<version>` (set by
# git_tag_name in release-plz.toml). The 18 runtime crates are git_only
# (publish = false), so release-plz uses their git tags - not a registry - as the
# "last released" baseline. With no per-package tags present, the FIRST release-plz
# run would treat every runtime as an initial release and build all 18 images.
#
# Run this ONCE at activation (after merging the #01-B PR, before the next push to
# main) to seed `<crate>-v<version>` for every workspace member at the last shared
# release commit, so release-plz's first run no-ops for unchanged crates.
#
# Idempotent: skips tags that already exist; safe to re-run.
#
# Usage:
#   scripts/seed-release-tags.sh [BASE_REF]   # BASE_REF defaults to v0.19.1
#   REMOTE=upstream scripts/seed-release-tags.sh
set -euo pipefail

base_ref="${1:-v0.19.1}"
remote="${REMOTE:-origin}"

if ! git rev-parse -q --verify "${base_ref}^{commit}" >/dev/null; then
  echo "error: base ref '${base_ref}' not found (expected the last shared release tag)" >&2
  exit 1
fi
base_commit="$(git rev-parse "${base_ref}^{commit}")"
echo "seeding per-package tags at ${base_ref} (${base_commit})"

desired=()
# cargo metadata -> "<name> <version>" for every workspace member. Build the FULL
# desired tag set; create any missing local tags, and verify pre-existing ones
# point at base_commit (a tag at a different commit is a hard error, not a silent
# skip).
while read -r name version; do
  tag="${name}-v${version}"
  desired+=("${tag}")
  if existing="$(git rev-parse -q --verify "refs/tags/${tag}^{commit}" 2>/dev/null)"; then
    if [[ "${existing}" != "${base_commit}" ]]; then
      echo "error: tag ${tag} already exists at ${existing}, expected ${base_commit}" >&2
      echo "       refusing to move it; resolve manually before re-running" >&2
      exit 1
    fi
    echo "  ok   ${tag} (already at base)"
  else
    git tag "${tag}" "${base_commit}"
    echo "  tag  ${tag} -> ${base_commit}"
  fi
done < <(
  cargo metadata --format-version 1 --no-deps \
    | python3 -c 'import json, sys
meta = json.load(sys.stdin)
members = set(meta["workspace_members"])
for pkg in meta["packages"]:
    if pkg["id"] in members:
        print(pkg["name"], pkg["version"])'
)

# Always push the full desired set, never just the newly-created locals: pushing
# an already-present identical tag is a no-op, so this converges the remote even
# after an earlier run created local tags but failed to push them.
echo "pushing ${#desired[@]} tag(s) to ${remote} (idempotent)..."
git push "${remote}" "${desired[@]}"
echo "done: ${#desired[@]} per-package tag(s) present on ${remote}."
