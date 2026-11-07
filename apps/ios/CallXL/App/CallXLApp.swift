import SwiftUI
import UIKit

enum AppPaths {
    /// `data_dir` = Library/Application Support/CallXL, `download_dir` = Documents (visible
    /// in the Files app: "On My iPhone › CallXL").
    static var dataDir: URL {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? URL(fileURLWithPath: NSTemporaryDirectory())
        return base.appendingPathComponent("CallXL", isDirectory: true)
    }

    static var documents: URL {
        FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first
            ?? URL(fileURLWithPath: NSTemporaryDirectory())
    }

    @MainActor
    static func config() -> AppConfig {
        try? FileManager.default.createDirectory(at: dataDir, withIntermediateDirectories: true)
        return AppConfig(
            dataDir: dataDir.path,
            downloadDir: documents.path,
            deviceName: UIDevice.current.name,
            signalingUrl: nil,
            headless: false
        )
    }
}

enum LaunchMode {
    static let arguments = ProcessInfo.processInfo.arguments
    /// UI smoke tests: in-app fake core, no CallKit.
    static var fakeCore: Bool {
        arguments.contains("-uiTestFakeCore")
    }

    /// Hosting the unit tests: the tests make their own cores.
    static var unitTestHost: Bool {
        ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] != nil
    }

    static var reportLaunch: Bool {
        arguments.contains("-uiTestReportLaunch")
    }
}

/// Milliseconds since this process started (kernel start time), for the §1.5 launch check.
enum LaunchClock {
    static func msSinceProcessStart() -> Double {
        var info = kinfo_proc()
        var size = MemoryLayout<kinfo_proc>.stride
        var mib: [Int32] = [CTL_KERN, KERN_PROC, KERN_PROC_PID, getpid()]
        guard sysctl(&mib, 4, &info, &size, nil, 0) == 0 else { return -1 }
        let start = info.kp_proc.p_un.__p_starttime
        let startS = Double(start.tv_sec) + Double(start.tv_usec) / 1_000_000
        return (Date().timeIntervalSince1970 - startS) * 1000
    }
}

@main
struct CallXLApp: App {
    @StateObject private var model: AppModel
    @AppStorage("appearance") private var appearance = Appearance.dark.rawValue

    init() {
        let core: CallXlProtocol = if LaunchMode.fakeCore || LaunchMode.unitTestHost {
            FakeCore()
        } else {
            CallXl(config: AppPaths.config())
        }
        _model = StateObject(wrappedValue: AppModel(core: core, useCallKit: !LaunchMode.fakeCore && !LaunchMode.unitTestHost))
    }

    var body: some Scene {
        WindowGroup {
            RootView()
                .environmentObject(model)
                .themed()
                .preferredColorScheme((Appearance(rawValue: appearance) ?? .dark).colorScheme)
                .onReceive(NotificationCenter.default.publisher(for: UIApplication.willTerminateNotification)) { _ in
                    model.shutdown()
                }
        }
    }
}
