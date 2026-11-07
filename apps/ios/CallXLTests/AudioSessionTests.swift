import AVFoundation
@testable import CallXL
import XCTest

/// M3: §8 audio session configuration, asserted on the Simulator's real AVAudioSession.
final class AudioSessionTests: XCTestCase {
    func testCallConfigurationOnSystemSession() throws {
        let session = AVAudioSession.sharedInstance()
        try CallAudio.configure(session)
        XCTAssertEqual(session.category, .playAndRecord)
        XCTAssertEqual(session.mode, .voiceChat)
        XCTAssertTrue(session.categoryOptions.contains(.allowBluetooth))
        XCTAssertTrue(session.categoryOptions.contains(.allowBluetoothA2DP))
        XCTAssertTrue(session.categoryOptions.contains(.defaultToSpeaker))
    }

    final class RecordingSession: AudioSessionConfiguring {
        var calls: [String] = []
        func setCategory(_ category: AVAudioSession.Category, mode: AVAudioSession.Mode, options: AVAudioSession.CategoryOptions) throws {
            calls.append("category \(category.rawValue) \(mode.rawValue) \(options.rawValue)")
        }

        func setActive(_ active: Bool, options _: AVAudioSession.SetActiveOptions) throws {
            calls.append("active \(active)")
        }
    }

    @MainActor
    func testLifecycleWithoutCallKit() {
        let rec = RecordingSession()
        let audio = CallAudioController(session: rec)
        audio.configureForCall()
        audio.activateWithoutCallKit()
        audio.callEnded(callKit: false)
        XCTAssertEqual(rec.calls, [
            "category \(AVAudioSession.Category.playAndRecord.rawValue) \(AVAudioSession.Mode.voiceChat.rawValue) \(CallAudio.options.rawValue)",
            "active true",
            "active false",
        ])
    }
}
