import AVFoundation
@testable import CallXL
import XCTest

/// M1: the UniFFI binding of the real Rust core, on the Simulator.
final class BindingsTests: XCTestCase {
    final class Recorder: EventListener, @unchecked Sendable {
        let sessionsChanged: XCTestExpectation
        private let lock = NSLock()
        private(set) var names: [String] = []

        init(_ e: XCTestExpectation) {
            sessionsChanged = e
        }

        func onEvent(event: Event) {
            lock.lock()
            names.append("\(event)".components(separatedBy: "(").first ?? "")
            lock.unlock()
            if case let .sessionsChanged(sessions) = event, sessions.contains(where: { $0.name == "Bindings" }) {
                sessionsChanged.fulfill()
            }
        }
    }

    /// Runs `work` on a background queue while the main run loop keeps spinning.
    private func offMain(_ work: @escaping () throws -> Void) throws {
        let done = expectation(description: "off-main work")
        var failure: Error?
        DispatchQueue.global(qos: .userInitiated).async {
            do { try work() } catch { failure = error }
            done.fulfill()
        }
        wait(for: [done], timeout: 30)
        if let failure {
            throw failure
        }
    }

    func testCoreLifecycleThroughBindings() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("callxl-bindings-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let config = AppConfig(
            dataDir: dir.path, downloadDir: dir.appendingPathComponent("Downloads").path,
            deviceName: "Simulator", signalingUrl: nil, headless: false
        )
        let core = CallXl(config: config)
        let expectation = expectation(description: "SessionsChanged")
        let recorder = Recorder(expectation)
        core.subscribe(listener: recorder)

        // Like the app: start/stop off the main thread (libwebrtc's iOS audio code may
        // need the main queue while they run).
        let t0 = Date()
        try offMain { try core.start() }
        print("bindings: start() took \(Int(Date().timeIntervalSince(t0) * 1000)) ms")

        let card = core.deviceCard()
        XCTAssertEqual(Data(hex: card.signingKey)?.count, 32)
        XCTAssertEqual(Data(hex: card.agreementKey)?.count, 32)
        XCTAssertEqual(card.displayName, "Simulator")

        let snap = core.snapshot()
        XCTAssertTrue(snap.sessions.isEmpty)
        XCTAssertNil(snap.currentSession)
        XCTAssertEqual(snap.callState, .idle)
        XCTAssertEqual(snap.settings.bitrateCeilingKbps, 510)
        XCTAssertEqual(snap.settings.apm, ApmConfig(aec: true, noiseSuppression: true, nsLevel: .moderate, highPass: true, agc: true))
        XCTAssertTrue(core.listAudioDevices().allSatisfy { !$0.id.isEmpty })

        let t1 = Date()
        let id = core.createSession(name: "Bindings")
        XCTAssertFalse(id.isEmpty)
        wait(for: [expectation], timeout: 2)
        print("bindings: SessionsChanged after \(Int(Date().timeIntervalSince(t1) * 1000)) ms; events \(recorder.names)")

        core.leaveSession()
        let report = core.exportDiagnostics()
        let json = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(report.utf8)) as? [String: Any])
        XCTAssertEqual(json["app"] as? String, "CallXL")
        // Diagnostics export schema v1 (docs/diagnostics-export.md).
        XCTAssertEqual(json["schema"] as? Int, 1)
        XCTAssertNotNil(json["global"])

        let t2 = Date()
        try offMain { core.stop() }
        print("bindings: stop() took \(Int(Date().timeIntervalSince(t2) * 1000)) ms")
        XCTAssertThrowsError(try offMain { try core.start() }) // a stopped core cannot start again
    }

    /// The Objective-C side of libwebrtc (RTCAudioSession) is loaded from CallXLCore
    /// (linked with -ObjC) and takes §8's configuration.
    func testWebRTCAudioSessionClassesAreLoadedAndConfigured() {
        XCTAssertNotNil(WebRTCAudioBridge.sessionClass)
        XCTAssertNotNil(WebRTCAudioBridge.shared)
        XCTAssertTrue(WebRTCAudioBridge.applyCallConfiguration())
        let options = WebRTCAudioBridge.configuredOptions ?? []
        XCTAssertTrue(options.contains(.defaultToSpeaker))
        XCTAssertTrue(options.contains(.allowBluetooth))
        XCTAssertEqual(WebRTCAudioBridge.webRTCConfiguration?.value(forKey: "mode") as? String, AVAudioSession.Mode.voiceChat.rawValue)
        WebRTCAudioBridge.setAudioEnabled(true)
        XCTAssertTrue(WebRTCAudioBridge.audioEnabled)
        WebRTCAudioBridge.setAudioEnabled(false)
        XCTAssertFalse(WebRTCAudioBridge.audioEnabled)
    }
}
