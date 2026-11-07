#!/usr/bin/env bash
# Builds CallXLCore.xcframework (iOS device arm64 + iOS Simulator arm64) from the Rust
# core and generates the UniFFI Swift binding into apps/ios/CallXL/Generated/.
#
# Linkage (DECISIONS.md, p2/ios): callxl-core is linked as a *dynamic* framework
# (`cargo rustc --crate-type cdylib`), so the linker resolves and dead-strips the 500 MB
# prebuilt libwebrtc.a once per slice and the app embeds one small Mach-O dylib.
#
# Runs on macOS with Xcode. Usage: packaging/ios/build-xcframework.sh
# Env: CALLXL_APP_SECRET (optional, baked in by crates/core/build.rs),
#      CALLXL_WEBRTC_CACHE (optional, cache root used by scripts/ci-webrtc-cache.sh).
# Output: target/ios/CallXLCore.xcframework, target/ios/CallXLCore.xcframework.zip,
#         apps/ios/CallXL/Generated/CallXLCore.swift
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT"

export IPHONEOS_DEPLOYMENT_TARGET=16.0
# Apple clang for the iOS SDK (the workflow-level CC=clang-21 is for Linux only).
export CC=clang CXX=clang++
TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
OUT="$ROOT/target/ios"
GEN="$ROOT/apps/ios/CallXL/Generated"
FW=CallXLCore
# Rust target -> LiveKit prebuilt triple. The prebuilt has no x86_64 simulator build
# (webrtc-ios-simulator-x64-release.zip is a 404 for webrtc-a545de4), so the simulator
# slice is arm64 only (Apple-silicon Macs, including the macos-latest runner).
TARGETS=(aarch64-apple-ios aarch64-apple-ios-sim)
triple_for() {
  case "$1" in
    aarch64-apple-ios) echo ios-device-arm64-release ;;
    aarch64-apple-ios-sim) echo ios-simulator-arm64-release ;;
    *) echo "unknown target $1" >&2; exit 1 ;;
  esac
}

rustup target add "${TARGETS[@]}"

rm -rf "$OUT"
mkdir -p "$OUT" "$GEN"

for t in "${TARGETS[@]}"; do
  triple="$(triple_for "$t")"
  lk="$(scripts/ci-webrtc-cache.sh "$triple" | tail -1)"
  echo "== $t ($lk)"
  # Unstripped so uniffi-bindgen can read the metadata; stripped below.
  env "$lk" CARGO_PROFILE_RELEASE_STRIP=false \
    cargo rustc -p callxl-core --lib --crate-type cdylib --release --target "$t" -- \
      -C link-arg=-Wl,-install_name,@rpath/$FW.framework/$FW \
      -C link-arg=-ObjC \
      -C link-arg=-Wl,-dead_strip \
      -C link-arg=-Wl,-dead_strip_dylibs
  lib="$TARGET_DIR/$t/release/libcallxl_core.dylib"
  ls -l "$lib"
  file "$lib"
done

# Swift binding (host bindgen, standalone crate: no host libwebrtc build needed).
BINDGEN_TARGET="$TARGET_DIR/bindgen-host"
cargo build --release --manifest-path packaging/ios/uniffi-bindgen/Cargo.toml --target-dir "$BINDGEN_TARGET"
SWIFT_OUT="$OUT/swift"
"$BINDGEN_TARGET/release/uniffi-bindgen" generate \
  --library "$TARGET_DIR/aarch64-apple-ios/release/libcallxl_core.dylib" \
  --language swift --no-format --out-dir "$SWIFT_OUT"
ls -l "$SWIFT_OUT"
cp "$SWIFT_OUT/CallXLCore.swift" "$GEN/CallXLCore.swift"

make_framework() {
  local t="$1" platform="$2"
  local dir="$OUT/$t/$FW.framework"
  mkdir -p "$dir/Headers" "$dir/Modules"
  cp "$TARGET_DIR/$t/release/libcallxl_core.dylib" "$dir/$FW"
  # Local symbols only; the exported uniffi_* entry points stay.
  strip -x "$dir/$FW"
  install_name_tool -id "@rpath/$FW.framework/$FW" "$dir/$FW"
  cp "$SWIFT_OUT/CallXLCoreFFI.h" "$dir/Headers/"
  cat > "$dir/Modules/module.modulemap" <<EOF
framework module $FW {
    header "CallXLCoreFFI.h"
    export *
}
EOF
  cat > "$dir/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleExecutable</key><string>$FW</string>
  <key>CFBundleIdentifier</key><string>com.callxl.app.core</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>$FW</string>
  <key>CFBundlePackageType</key><string>FMWK</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundleVersion</key><string>1</string>
  <key>CFBundleSupportedPlatforms</key><array><string>$platform</string></array>
  <key>MinimumOSVersion</key><string>$IPHONEOS_DEPLOYMENT_TARGET</string>
</dict>
</plist>
EOF
  echo "$dir: $(du -sh "$dir/$FW" | cut -f1)"
  otool -L "$dir/$FW" | head -30
}
make_framework aarch64-apple-ios iPhoneOS
make_framework aarch64-apple-ios-sim iPhoneSimulator

xcodebuild -create-xcframework \
  -framework "$OUT/aarch64-apple-ios/$FW.framework" \
  -framework "$OUT/aarch64-apple-ios-sim/$FW.framework" \
  -output "$OUT/$FW.xcframework"
(cd "$OUT" && zip -qry "$FW.xcframework.zip" "$FW.xcframework")
ls -l "$OUT/$FW.xcframework.zip"
find "$OUT/$FW.xcframework" -maxdepth 3 | sort
