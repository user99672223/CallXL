import AVFoundation
import CallKit
@testable import CallXL
import XCTest

/// M3: the CallKit wiring, driven through the CXProviderDelegate call sequence CallKit
/// itself would produce (start → connected → mute → hold → resume → end).
@MainActor
final class CallKitTests: XCTestCase {
    final class Commands: CallCommands {
        var log: [String] = []
        func performJoin(_ target: JoinTarget) {
            log.append("join \(target)")
        }

        func performLeave() {
            log.append("leave")
        }

        func performSetMuted(_ muted: Bool) {
            log.append("muted \(muted)")
        }

        func performHold(_ onHold: Bool) {
            log.append("hold \(onHold)")
        }

        func audioSessionActivated(_: AVAudioSession) {
            log.append("activated")
        }

        func audioSessionDeactivated(_: AVAudioSession) {
            log.append("deactivated")
        }

        func callKitUnavailable(_ target: JoinTarget) {
            log.append("no-callkit \(target)")
        }
    }

    final class Reporter: CallReporting {
        var log: [String] = []
        func reportConnecting(_: UUID) {
            log.append("connecting")
        }

        func reportConnected(_: UUID) {
            log.append("connected")
        }

        func reportEnded(_: UUID, reason: CXCallEndedReason) {
            log.append("ended \(reason.rawValue)")
        }
    }

    /// Plays CallKit: performs each requested action on the delegate.
    final class Transport: CallTransactionRequesting {
        weak var controller: CallController?
        let provider = CXProvider(configuration: CallController.configuration())
        var fail = false

        func request(_ transaction: CXTransaction, completion: @escaping (Error?) -> Void) {
            if fail {
                completion(NSError(domain: CXErrorDomainRequestTransaction, code: 1))
                return
            }
            guard let c = controller else { return }
            for action in transaction.actions {
                switch action {
                case let a as CXStartCallAction: c.provider(provider, perform: a)
                case let a as CXEndCallAction: c.provider(provider, perform: a)
                case let a as CXSetMutedCallAction: c.provider(provider, perform: a)
                case let a as CXSetHeldCallAction: c.provider(provider, perform: a)
                default: XCTFail("unexpected action \(action)")
                }
            }
            completion(nil)
        }
    }

    func testCallSequence() {
        let commands = Commands()
        let reporter = Reporter()
        let transport = Transport()
        let calls = CallController(commands: commands, reporter: reporter, transport: transport)
        transport.controller = calls

        calls.startCall(.create(name: "Kitchen"), title: "Kitchen")
        XCTAssertTrue(calls.hasCall)
        let uuid = try? XCTUnwrap(calls.callUUID)
        calls.provider(transport.provider, didActivate: AVAudioSession.sharedInstance())
        calls.callConnected()
        calls.callConnected() // reported once
        calls.setMuted(true)
        // A phone call arrives: CallKit holds ours, then resumes it.
        calls.provider(transport.provider, perform: CXSetHeldCallAction(call: uuid ?? UUID(), onHold: true))
        calls.provider(transport.provider, didDeactivate: AVAudioSession.sharedInstance())
        calls.provider(transport.provider, perform: CXSetHeldCallAction(call: uuid ?? UUID(), onHold: false))
        calls.provider(transport.provider, didActivate: AVAudioSession.sharedInstance())
        calls.endCall()
        XCTAssertFalse(calls.hasCall)
        calls.callEnded(reason: nil) // the core's Idle afterwards reports nothing more

        XCTAssertEqual(commands.log, [
            "join create(name: Optional(\"Kitchen\"))",
            "activated",
            "muted true",
            "hold true",
            "deactivated",
            "hold false",
            "activated",
            "leave",
        ])
        XCTAssertEqual(reporter.log, ["connecting", "connected"])
    }

    func testCoreEndedCallIsReported() {
        let commands = Commands()
        let reporter = Reporter()
        let transport = Transport()
        let calls = CallController(commands: commands, reporter: reporter, transport: transport)
        transport.controller = calls
        calls.startCall(.existing(id: "abc"), title: "x")
        calls.callEnded(reason: "connection lost for 30 s")
        XCTAssertEqual(reporter.log, ["connecting", "ended \(CXCallEndedReason.failed.rawValue)"])
        XCTAssertFalse(calls.hasCall)
    }

    func testFallsBackWhenCallKitRefuses() {
        let commands = Commands()
        let transport = Transport()
        transport.fail = true
        let calls = CallController(commands: commands, reporter: Reporter(), transport: transport)
        calls.startCall(.existing(id: "abc"), title: "x")
        XCTAssertEqual(commands.log, ["no-callkit existing(id: \"abc\")"])
        XCTAssertFalse(calls.hasCall)
    }
}
