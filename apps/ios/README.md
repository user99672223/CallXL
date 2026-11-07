# CallXL for iOS

SwiftUI app over the shared Rust core (`CallXLCore.xcframework`, UniFFI Swift binding).
iOS 16 or later, iPhone. Sideloaded; there is no App Store build.

## Install (sideload)

1. Download `CallXL-unsigned.ipa` from the GitHub Release (or the `CallXL-unsigned.ipa`
   artifact of a CI run).
2. Install it with [AltStore](https://altstore.io) or [Sideloadly](https://sideloadly.io)
   using your Apple ID. A free Apple ID works; the app is signed on your machine.
3. On the iPhone: Settings › General › VPN & Device Management › trust your Apple ID's
   developer profile.
4. First launch asks for **Local Network** access (needed to find and call devices on your
   Wi-Fi) and, on the first call, for the **Microphone**. Allow both.

With a free Apple ID the signature expires after **7 days**: re-sign/refresh in AltStore
(it can do this automatically over Wi-Fi) or re-install with Sideloadly. Refreshing with
the same Apple ID keeps the app's data and identity keys; installing with a different
Apple ID gives the app a new keychain, so peers see a "Key changed — trust again?" prompt.

## What works without push notifications

CallXL deliberately has no PushKit/APNs (they need a paid developer account), so:

- You can be called only while CallXL is **open or already in a call**. When the app is
  closed or suspended, nobody can ring you; open the app and the session list shows who is
  in which session.
- During a call, CallXL keeps running in the background (background audio mode) and shows
  the system call UI via CallKit (lock screen, Dynamic Island, Bluetooth controls).
- A cellular/FaceTime call puts the CallXL call on hold (microphone muted) and resumes it
  afterwards; ending the CallXL call from the system UI leaves the session.
- People joining a session you are in simply appear — that is not an "incoming call".

## Where things are

- Received files: **Files › On My iPhone › CallXL** (the app's Documents folder). File
  cards in the chat have **Share** and **Open in Files**.
- Diagnostics › Export report writes a JSON report and opens the share sheet.
- Settings: audio processing toggles and **Raw mic**, bitrate ceiling (default 510 kbps),
  display name, trusted devices, signaling URL, theme.

## Build (macOS with Xcode)

```sh
packaging/ios/build-xcframework.sh        # Rust core -> target/ios/CallXLCore.xcframework
                                           # + apps/ios/CallXL/Generated/CallXLCore.swift
brew install xcodegen swiftformat
(cd apps/ios && xcodegen generate)         # apps/ios/CallXL.xcodeproj
xcodebuild test -project apps/ios/CallXL.xcodeproj -scheme CallXL \
  -destination 'platform=iOS Simulator,name=iPhone 16'
packaging/ios/build-unsigned-ipa.sh        # target/ios/CallXL-unsigned.ipa
```

The simulator slice is arm64 only (Apple-silicon Macs); see `DECISIONS.md` D-0039.
Design tokens come from `design/tokens.json`; after changing it run
`packaging/ios/gen-tokens.py` (CI checks that `Theme/Tokens.swift` is current).
`swiftformat --lint apps/ios` must be clean.

Test-only launch arguments: `-uiTestFakeCore` (in-app fake core, no CallKit) and
`-uiTestReportLaunch` (shows the launch-to-Home time for the UI test).
