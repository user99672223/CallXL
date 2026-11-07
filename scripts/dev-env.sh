# Source this file: `. scripts/dev-env.sh`
# Shared cargo target dir for all worktrees on the dev machine (D-0006).
export CARGO_TARGET_DIR=/home/user/target-shared
# libwebrtc's hermetic libc++ needs clang >= 21 (see DECISIONS.md).
export CC=clang-21
export CXX=clang++-21
# Prebuilt libwebrtc, downloaded once by scripts/ci-webrtc-cache.sh.
_lk="$(dirname "${BASH_SOURCE[0]}")/ci-webrtc-cache.sh"
export "$("$_lk" | tail -1)"
unset _lk
