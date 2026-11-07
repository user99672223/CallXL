import XCTest

/// M3: the shipped Info.plist keys (§8: background audio, mic, Bonjour on the local
/// network, Files visibility; no push).
final class InfoPlistTests: XCTestCase {
    func testInfoPlist() throws {
        let info = try XCTUnwrap(Bundle.main.infoDictionary)
        XCTAssertEqual(Bundle.main.bundleIdentifier, "com.callxl.app")
        XCTAssertEqual(info["UIBackgroundModes"] as? [String], ["audio"])
        XCTAssertFalse((info["NSMicrophoneUsageDescription"] as? String ?? "").isEmpty)
        XCTAssertFalse((info["NSLocalNetworkUsageDescription"] as? String ?? "").isEmpty)
        XCTAssertEqual(info["NSBonjourServices"] as? [String], ["_callxl._tcp"])
        XCTAssertEqual(info["UIFileSharingEnabled"] as? Bool, true)
        XCTAssertEqual(info["LSSupportsOpeningDocumentsInPlace"] as? Bool, true)
        XCTAssertEqual(info["MinimumOSVersion"] as? String, "16.0")
        XCTAssertNil(info["aps-environment"])
    }

    func testCoreFrameworkIsEmbedded() throws {
        let fw = try XCTUnwrap(Bundle.main.privateFrameworksURL).appendingPathComponent("CallXLCore.framework/CallXLCore")
        XCTAssertTrue(FileManager.default.fileExists(atPath: fw.path))
    }
}
