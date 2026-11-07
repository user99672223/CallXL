#!/usr/bin/env bash
# Download LiveKit's prebuilt libwebrtc once into a cache directory and print the
# LK_CUSTOM_WEBRTC export that points webrtc-sys at it.
#
# Usage: scripts/ci-webrtc-cache.sh [triple]     (default triple: linux-x64-release)
# Output (stdout, last line): LK_CUSTOM_WEBRTC=<dir>
# CI caches $HOME/.cache/callxl/<tag> keyed on the tag (actions/cache).
set -euo pipefail

TAG="webrtc-a545de4"           # must match webrtc-sys-build's WEBRTC_TAG
TRIPLE="${1:-linux-x64-release}"
CACHE_ROOT="${CALLXL_WEBRTC_CACHE:-$HOME/.cache/callxl}"
DEST="$CACHE_ROOT/$TAG/$TRIPLE"
URL="https://github.com/livekit/rust-sdks/releases/download/$TAG/webrtc-$TRIPLE.zip"

if [[ ! -f "$DEST/include/api/peer_connection_interface.h" || ! ( -f "$DEST/lib/libwebrtc.a" || -f "$DEST/lib/webrtc.lib" ) ]]; then
  echo "downloading $URL" >&2
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT
  curl -fL --retry 5 --retry-delay 3 -o "$tmp/webrtc.zip" "$URL" >&2
  mkdir -p "$tmp/x"
  unzip -q "$tmp/webrtc.zip" -d "$tmp/x"
  # The archive contains a single top-level directory named after the triple.
  src="$(find "$tmp/x" -mindepth 1 -maxdepth 1 -type d | head -1)"
  rm -rf "$DEST"
  mkdir -p "$(dirname "$DEST")"
  mv "$src" "$DEST"
  chmod -R u+rwX "$DEST"
fi

# Windows (Git Bash): build scripts need a native path, not /c/Users/….
if command -v cygpath >/dev/null 2>&1; then
  DEST="$(cygpath -m "$DEST")"
fi

if [[ -n "${GITHUB_ENV:-}" ]]; then
  echo "LK_CUSTOM_WEBRTC=$DEST" >> "$GITHUB_ENV"
fi
echo "LK_CUSTOM_WEBRTC=$DEST"
