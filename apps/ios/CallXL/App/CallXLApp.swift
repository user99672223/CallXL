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

@main
struct CallXLApp: App {
    @StateObject private var model: AppModel
    @AppStorage("appearance") private var appearance = Appearance.dark.rawValue

    init() {
        LaunchClock.mark("app.init")
        let fake = LaunchMode.fakeCore || LaunchMode.unitTestHost
        let config = fake ? nil : AppPaths.config()
        _model = StateObject(wrappedValue: AppModel(useCallKit: !fake) {
            if let config {
                return CallXl(config: config)
            }
            return FakeCore()
        })
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
