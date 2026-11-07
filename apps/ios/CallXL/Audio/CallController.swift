import AVFoundation
import CallKit
import os

/// What CallKit asks the app to do (implemented by `AppModel`).
@MainActor
protocol CallCommands: AnyObject {
    func performJoin(_ target: JoinTarget)
    func performLeave()
    func performSetMuted(_ muted: Bool)
    func performHold(_ onHold: Bool)
    func audioSessionActivated(_ session: AVAudioSession)
    func audioSessionDeactivated(_ session: AVAudioSession)
    func callKitUnavailable(_ target: JoinTarget)
}

/// `CXProvider` reporting calls (a recorder in tests).
protocol CallReporting: AnyObject {
    func reportConnecting(_ uuid: UUID)
    func reportConnected(_ uuid: UUID)
    func reportEnded(_ uuid: UUID, reason: CXCallEndedReason)
}

/// `CXCallController` transactions (in tests: performs the actions directly).
protocol CallTransactionRequesting: AnyObject {
    func request(_ transaction: CXTransaction, completion: @escaping (Error?) -> Void)
}

final class SystemCallReporter: CallReporting {
    let provider: CXProvider

    init(provider: CXProvider) {
        self.provider = provider
    }

    func reportConnecting(_ uuid: UUID) {
        provider.reportOutgoingCall(with: uuid, startedConnectingAt: Date())
    }

    func reportConnected(_ uuid: UUID) {
        provider.reportOutgoingCall(with: uuid, connectedAt: Date())
    }

    func reportEnded(_ uuid: UUID, reason: CXCallEndedReason) {
        provider.reportCall(with: uuid, endedAt: Date(), reason: reason)
    }
}

final class SystemCallTransport: CallTransactionRequesting {
    private let controller = CXCallController()

    func request(_ transaction: CXTransaction, completion: @escaping (Error?) -> Void) {
        controller.request(transaction) { error in
            DispatchQueue.main.async { completion(error) }
        }
    }
}

/// CallKit integration (§8): joining a session is an outgoing call in the system call UI;
/// CallKit owns audio-session activation and interruptions (a phone call puts ours on
/// hold, which mutes the microphone until it resumes). There is no PushKit, so there are
/// no incoming calls: a peer joining a session we are in is not an incoming call.
/// All delegate callbacks arrive on the main queue.
final class CallController: NSObject, CXProviderDelegate {
    private let log = Logger(subsystem: "com.callxl.app", category: "callkit")
    weak var commands: CallCommands?
    private let reporter: CallReporting
    private let transport: CallTransactionRequesting
    private let provider: CXProvider?
    private(set) var callUUID: UUID?
    private var pending: [UUID: JoinTarget] = [:]
    private var connectedReported = false

    var hasCall: Bool {
        callUUID != nil
    }

    static func configuration() -> CXProviderConfiguration {
        let c = CXProviderConfiguration()
        c.supportsVideo = false
        c.maximumCallGroups = 1
        c.maximumCallsPerCallGroup = 1
        c.supportedHandleTypes = [.generic]
        c.includesCallsInRecents = false
        return c
    }

    /// Production: the real `CXProvider` and `CXCallController`.
    init(commands: CallCommands?) {
        let provider = CXProvider(configuration: Self.configuration())
        self.provider = provider
        reporter = SystemCallReporter(provider: provider)
        transport = SystemCallTransport()
        self.commands = commands
        super.init()
        provider.setDelegate(self, queue: nil)
    }

    /// Tests: injected reporter/transport, no system provider.
    init(commands: CallCommands?, reporter: CallReporting, transport: CallTransactionRequesting) {
        provider = nil
        self.reporter = reporter
        self.transport = transport
        self.commands = commands
        super.init()
    }

    // MARK: - Requests from the UI

    func startCall(_ target: JoinTarget, title: String) {
        let uuid = UUID()
        pending[uuid] = target
        let action = CXStartCallAction(call: uuid, handle: CXHandle(type: .generic, value: title))
        action.contactIdentifier = title
        transport.request(CXTransaction(action: action)) { [weak self] error in
            guard let self, let error else { return }
            failed(uuid, error)
        }
    }

    private func failed(_ uuid: UUID, _ error: Error) {
        log.error("CallKit start failed: \(error.localizedDescription, privacy: .public)")
        if let target = pending.removeValue(forKey: uuid) {
            MainActor.assumeIsolated { commands?.callKitUnavailable(target) }
        }
    }

    func endCall() {
        guard let uuid = callUUID else {
            MainActor.assumeIsolated { commands?.performLeave() }
            return
        }
        transport.request(CXTransaction(action: CXEndCallAction(call: uuid))) { [weak self] error in
            guard let self, error != nil else { return }
            callUUID = nil
            MainActor.assumeIsolated { self.commands?.performLeave() }
        }
    }

    func setMuted(_ muted: Bool) {
        guard let uuid = callUUID else {
            MainActor.assumeIsolated { commands?.performSetMuted(muted) }
            return
        }
        transport.request(CXTransaction(action: CXSetMutedCallAction(call: uuid, muted: muted))) { [weak self] error in
            guard let self, error != nil else { return }
            MainActor.assumeIsolated { self.commands?.performSetMuted(muted) }
        }
    }

    // MARK: - Core call state

    func callConnected() {
        guard let uuid = callUUID, !connectedReported else { return }
        connectedReported = true
        reporter.reportConnected(uuid)
    }

    /// The core left the session (we left, or it gave up reconnecting).
    func callEnded(reason: String?) {
        guard let uuid = callUUID else { return }
        callUUID = nil
        connectedReported = false
        reporter.reportEnded(uuid, reason: (reason?.isEmpty ?? true) || reason == "left" ? .remoteEnded : .failed)
    }

    // MARK: - CXProviderDelegate

    func providerDidReset(_: CXProvider) {
        callUUID = nil
        connectedReported = false
        MainActor.assumeIsolated { commands?.performLeave() }
    }

    func provider(_: CXProvider, perform action: CXStartCallAction) {
        guard let target = pending.removeValue(forKey: action.callUUID) else {
            action.fail()
            return
        }
        callUUID = action.callUUID
        connectedReported = false
        MainActor.assumeIsolated { commands?.performJoin(target) }
        reporter.reportConnecting(action.callUUID)
        action.fulfill()
    }

    func provider(_: CXProvider, perform action: CXEndCallAction) {
        if action.callUUID == callUUID {
            callUUID = nil
            connectedReported = false
            MainActor.assumeIsolated { commands?.performLeave() }
        }
        action.fulfill()
    }

    func provider(_: CXProvider, perform action: CXSetMutedCallAction) {
        let muted = action.isMuted
        MainActor.assumeIsolated { commands?.performSetMuted(muted) }
        action.fulfill()
    }

    func provider(_: CXProvider, perform action: CXSetHeldCallAction) {
        let held = action.isOnHold
        MainActor.assumeIsolated { commands?.performHold(held) }
        action.fulfill()
    }

    func provider(_: CXProvider, didActivate audioSession: AVAudioSession) {
        MainActor.assumeIsolated { commands?.audioSessionActivated(audioSession) }
    }

    func provider(_: CXProvider, didDeactivate audioSession: AVAudioSession) {
        MainActor.assumeIsolated { commands?.audioSessionDeactivated(audioSession) }
    }
}
