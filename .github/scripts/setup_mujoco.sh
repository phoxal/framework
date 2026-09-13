#!/usr/bin/env bash
# Install the pinned native engine for local verification and CI.
set -euo pipefail
prefix="${1:?usage: setup_mujoco.sh INSTALL_DIRECTORY}"
version=3.12.0
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) archive="mujoco-$version-linux-x86_64.tar.gz" ;;
  Linux-aarch64) archive="mujoco-$version-linux-aarch64.tar.gz" ;;
  Darwin-*) archive="mujoco-$version-macos-universal2.dmg" ;;
  *) echo "Unsupported native host" >&2; exit 1 ;;
esac
mkdir -p "$prefix"
prefix="$(cd "$prefix" && pwd)"
download_dir="$(mktemp -d)"
trap 'rm -rf "$download_dir"' EXIT
base="https://github.com/google-deepmind/mujoco/releases/download/$version"
curl --fail --location --silent --show-error "$base/$archive" -o "$download_dir/$archive"
curl --fail --location --silent --show-error "$base/$archive.sha256" -o "$download_dir/$archive.sha256"
if [[ "$archive" == *.dmg ]]; then
  (cd "$download_dir" && shasum -a 256 --check "$archive.sha256")
  mkdir "$download_dir/mount"
  hdiutil attach "$download_dir/$archive" -mountpoint "$download_dir/mount" -nobrowse -quiet
  trap 'hdiutil detach "$download_dir/mount" -quiet || true; rm -rf "$download_dir"' EXIT
  mkdir -p "$prefix/lib"
  cp "$download_dir/mount/mujoco.framework/Versions/A/libmujoco.$version.dylib" "$prefix/lib/"
  ln -sf "libmujoco.$version.dylib" "$prefix/lib/libmujoco.dylib"
  library_variable=DYLD_LIBRARY_PATH
else
  (cd "$download_dir" && sha256sum --check "$archive.sha256")
  tar -xzf "$download_dir/$archive" -C "$download_dir"
  cp -R "$download_dir/mujoco-$version/lib" "$prefix/"
  library_variable=LD_LIBRARY_PATH
fi
if [[ -n "${GITHUB_ENV:-}" ]]; then
  echo "MUJOCO_DYNAMIC_LINK_DIR=$prefix/lib" >> "$GITHUB_ENV"
  echo "$library_variable=$prefix/lib" >> "$GITHUB_ENV"
fi
printf 'MUJOCO_DYNAMIC_LINK_DIR=%s/lib\n%s=%s/lib\n' "$prefix" "$library_variable" "$prefix"
