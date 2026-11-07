#!/usr/bin/env bash
# Unsigned CallXL.ipa for sideloading (CLAUDE.md §8): Release build for iphoneos with
# signing disabled -> Payload/CallXL.app -> zip -> target/ios/CallXL-unsigned.ipa.
# AltStore/Sideloadly sign it with the user's Apple ID when installing.
# Needs target/ios/CallXLCore.xcframework (build-xcframework.sh) and the generated
# Xcode project (xcodegen generate in apps/ios).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT="$ROOT/target/ios"
DERIVED="$OUT/DerivedData-ipa"
mkdir -p "$OUT"
rm -rf "$DERIVED" "$OUT/Payload" "$OUT/CallXL-unsigned.ipa"

xcodebuild \
  -project "$ROOT/apps/ios/CallXL.xcodeproj" \
  -scheme CallXL \
  -configuration Release \
  -sdk iphoneos \
  -destination 'generic/platform=iOS' \
  -derivedDataPath "$DERIVED" \
  CODE_SIGNING_ALLOWED=NO CODE_SIGNING_REQUIRED=NO CODE_SIGN_IDENTITY="" \
  build

APP="$DERIVED/Build/Products/Release-iphoneos/CallXL.app"
test -x "$APP/CallXL"
mkdir -p "$OUT/Payload"
cp -R "$APP" "$OUT/Payload/"
(cd "$OUT" && zip -qry CallXL-unsigned.ipa Payload)
rm -rf "$OUT/Payload"

ls -l "$OUT/CallXL-unsigned.ipa"
unzip -l "$OUT/CallXL-unsigned.ipa" | grep -E 'Payload/CallXL.app/(CallXL|Info.plist|Frameworks/CallXLCore.framework/CallXLCore)$'
du -sh "$APP" "$APP/CallXL" "$APP/Frameworks/CallXLCore.framework/CallXLCore"
