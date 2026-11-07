#!/usr/bin/env bash
# Fails unless the release tag matches the app version (CLAUDE.md §12 artifact names).
#   scripts/check-version.sh v0.1.0            compare
#   scripts/check-version.sh --stamp v0.1.0-rc1 prerelease tags only: write the tag's version
#                                              into the manifest first, then compare
# Version source: apps/desktop/Cargo.toml `version = "..."`, or the workspace
# `[workspace.package] version` when the desktop crate inherits it (or does not exist yet).
# Prints `version=<ver>` (also to $GITHUB_OUTPUT when set).
set -euo pipefail
cd "$(dirname "$0")/.."

stamp=0
if [[ "${1:-}" == "--stamp" ]]; then stamp=1; shift; fi
tag="${1:?usage: check-version.sh [--stamp] <tag>}"
want="${tag#v}"
[[ "$tag" == v* && "$want" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]] || {
  echo "::error::tag '$tag' is not v<major>.<minor>.<patch>[-<pre>]" >&2; exit 1; }

desktop=apps/desktop/Cargo.toml
manifest=Cargo.toml
section='workspace.package'
if [[ -f "$desktop" ]] && grep -qE '^version[[:space:]]*=[[:space:]]*"' "$desktop"; then
  manifest="$desktop"; section='package'
fi

read_version() {
  awk -v sec="[$section]" '
    /^\[/ { in_sec = ($0 == sec) }
    in_sec && /^version[[:space:]]*=/ { gsub(/.*=[[:space:]]*"|".*/, ""); print; exit }
  ' "$manifest"
}

if (( stamp )); then
  [[ "$want" == *-* ]] || { echo "::error::--stamp is only for prerelease tags (vX.Y.Z-pre), got $tag" >&2; exit 1; }
  awk -v sec="[$section]" -v ver="$want" '
    /^\[/ { in_sec = ($0 == sec) }
    in_sec && !done && /^version[[:space:]]*=/ { print "version = \"" ver "\""; done = 1; next }
    { print }
  ' "$manifest" > "$manifest.tmp" && mv "$manifest.tmp" "$manifest"
  echo "stamped $manifest [$section] version = $want"
fi

have="$(read_version)"
if [[ "$have" != "$want" ]]; then
  echo "::error::version mismatch: tag $tag wants $want but $manifest [$section] says '$have'" >&2
  exit 1
fi
echo "version=$have"
if [[ -n "${GITHUB_OUTPUT:-}" ]]; then echo "version=$have" >> "$GITHUB_OUTPUT"; fi
