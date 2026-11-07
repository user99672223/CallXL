import SwiftUI

/// Minimal SwiftUI app with no CallXL code: measures the Simulator's own launch floor with
/// the same clock and script as CallXL (packaging/ios/measure-launch.sh, DECISIONS.md).
@main
struct BaselineApp: App {
    init() {
        LaunchClock.mark("app.init")
        #if BASELINE_FRAMEWORKS
            _ = FrameworkUses.touch()
        #endif
    }

    var body: some Scene {
        WindowGroup {
            Text("Baseline")
                .onAppear {
                    LaunchClock.mark("home.appear")
                    LaunchClock.writeReport(LaunchClock.msSinceProcessStart())
                }
        }
    }
}
