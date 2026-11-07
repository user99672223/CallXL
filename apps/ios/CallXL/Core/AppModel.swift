import AVFoundation
import Foundation
import os
import SwiftUI

/// What joining means: create a new session (the creator joins it) or join a listed one.
enum JoinTarget: Equatable {
    case create(name: String?)
    case existing(id: String)
}

struct TrustPromptItem: Identifiable, Equatable {
    let peerKey: String
    let displayName: String
    let phrase: String
    let keyChanged: Bool
    var id: String {
        peerKey
    }
}

/// The only owner of UI state. Reads state from and sends commands to the core (§9);
/// every core call made here returns immediately (the core queues work on its own task),
/// and the two that can take long (`start`, `stop`) run off the main actor.
@MainActor
final class AppModel: ObservableObject {
    static let log = Logger(subsystem: "com.callxl.app", category: "app")

    let core: CallXlProtocol
    private(set) var calls: CallController?
    let audio: CallAudioController

    @Published private(set) var snapshot: Snapshot
    @Published private(set) var diagnostics: DiagnosticsSnapshot?
    @Published private(set) var chat: [ChatMessage] = []
    @Published var trustPrompts: [TrustPromptItem] = []
    @Published var fileOffers: [TransferInfo] = []
    @Published var lastError: String?
    @Published private(set) var started = false
    @Published private(set) var interrupted = false
    /// Speaking flags updated straight from `SpeakingChanged` (faster than snapshots).
    @Published private(set) var speaking: Set<String> = []
    /// Unread chat messages while the chat drawer is closed.
    @Published var unread = 0
    var chatOpen = false {
        didSet {
            if chatOpen {
                markAllRead()
            }
        }
    }

    private var userMuted = false
    private var held = false
    private var refreshTimer: Timer?
    private var chatSession: String?
    /// Outgoing files picked through the document picker stay security-scoped until done.
    private var scopedFiles: [String: URL] = [:]

    init(core: CallXlProtocol, useCallKit: Bool) {
        self.core = core
        snapshot = core.snapshot()
        audio = CallAudioController()
        if useCallKit {
            calls = CallController(commands: nil)
        }
        calls?.commands = self
        audio.prepareWebRTC(manualAudio: useCallKit)
        core.subscribe(listener: EventBridge { [weak self] event in
            self?.handle(event)
        })
        NotificationCenter.default.addObserver(
            forName: AVAudioSession.interruptionNotification, object: nil, queue: .main
        ) { [weak self] note in
            let began = (note.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt) == AVAudioSession.InterruptionType.began.rawValue
            MainActor.assumeIsolated {
                self?.interrupted = began
            }
        }
    }

    /// Starts the core off the main actor; Home is already on screen meanwhile.
    func start() {
        let core = core
        Task.detached(priority: .userInitiated) {
            do {
                try core.start()
                await MainActor.run { self.didStart() }
            } catch {
                let message = (error as? CoreError).map(Self.describe) ?? error.localizedDescription
                await MainActor.run { self.lastError = message }
            }
        }
    }

    private func didStart() {
        started = true
        refresh()
        refreshTimer?.invalidate()
        // State changes without an event (mute, settings) are picked up within a second.
        refreshTimer = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.refresh() }
        }
    }

    /// Leaves and stops the core; blocks up to 5 s, so never on the main actor.
    func shutdown() {
        let core = core
        Task.detached { core.stop() }
    }

    nonisolated static func describe(_ error: CoreError) -> String {
        switch error {
        case .AlreadyStarted: "CallXL is already running."
        case let .StartFailed(reason): "CallXL could not start: \(reason)"
        }
    }

    func refresh() {
        let next = core.snapshot()
        if next != snapshot {
            snapshot = next
            speaking = Set(next.peers.filter(\.speaking).map(\.key))
        }
    }

    // MARK: - Derived state

    var inCall: Bool {
        switch snapshot.callState {
        case .joining, .inCall, .reconnecting: true
        case .idle, .left: false
        }
    }

    var currentSession: SessionSummary? {
        guard let id = snapshot.currentSession else { return nil }
        return snapshot.sessions.first { $0.id == id }
    }

    var muted: Bool {
        userMuted || snapshot.muted
    }

    // MARK: - Events

    func handle(_ event: Event) {
        switch event {
        case let .sessionsChanged(sessions):
            snapshot.sessions = sessions
        case let .trustedDevicesChanged(devices):
            snapshot.trustedDevices = devices
        case let .callStateChanged(state):
            snapshot.callState = state
            callStateChanged(state)
        case let .speakingChanged(peerKey, isSpeaking):
            if isSpeaking {
                speaking.insert(peerKey)
            } else {
                speaking.remove(peerKey)
            }
            return
        case let .chatReceived(message):
            receive(message)
        case let .receiptUpdated(messageId, status, _):
            if let i = chat.firstIndex(where: { $0.id == messageId }), rank(status) > rank(chat[i].status) {
                chat[i].status = status
            }
        case let .fileOffered(transfer):
            if transfer.direction == .incoming, !fileOffers.contains(where: { $0.id == transfer.id }) {
                fileOffers.append(transfer)
            }
        case let .transferProgress(transfer):
            updateTransfer(transfer)
            return
        case let .trustPrompt(peerKey, displayName, phrase, keyChanged):
            trustPrompts.removeAll { $0.peerKey == peerKey }
            trustPrompts.append(TrustPromptItem(peerKey: peerKey, displayName: displayName, phrase: phrase, keyChanged: keyChanged))
        case let .statsTick(snap):
            diagnostics = snap
        case let .error(reason):
            lastError = reason
        case .peerJoined, .peerLeft, .peerPathChanged, .devicesChanged:
            break
        }
        refresh()
    }

    private func rank(_ s: ReceiptStatus) -> Int {
        switch s {
        case .sending: 0
        case .sent: 1
        case .delivered: 2
        case .read: 3
        }
    }

    private func callStateChanged(_ state: CallState) {
        switch state {
        case .inCall:
            calls?.callConnected()
            if let id = snapshot.currentSession, chatSession != id {
                loadChat(session: id)
            }
        case let .left(reason):
            calls?.callEnded(reason: reason)
            audio.callEnded(callKit: calls != nil)
            finishLeaving()
            if !reason.isEmpty, reason != "left" {
                lastError = reason
            }
        case .idle:
            calls?.callEnded(reason: nil)
            audio.callEnded(callKit: calls != nil)
            finishLeaving()
        case .joining, .reconnecting:
            break
        }
    }

    private func finishLeaving() {
        userMuted = false
        held = false
        speaking = []
    }

    // MARK: - Calls (UI entry points; CallKit routes them back through CallCommands)

    func createSession(name: String?) {
        let trimmed = name?.trimmingCharacters(in: .whitespacesAndNewlines)
        start(.create(name: (trimmed?.isEmpty ?? true) ? nil : trimmed))
    }

    func join(_ session: SessionSummary) {
        start(.existing(id: session.id))
    }

    private func start(_ target: JoinTarget) {
        if let calls {
            let title: String = switch target {
            case let .create(name): name ?? "CallXL session"
            case let .existing(id): snapshot.sessions.first { $0.id == id }?.name ?? "CallXL session"
            }
            calls.startCall(target, title: title)
        } else {
            performJoin(target)
            audio.activateWithoutCallKit()
        }
    }

    func leave() {
        if let calls, calls.hasCall {
            calls.endCall()
        } else {
            performLeave()
        }
    }

    func toggleMute() {
        let next = !muted
        if let calls, calls.hasCall {
            calls.setMuted(next)
        } else {
            performSetMuted(next)
        }
    }

    // MARK: - Chat

    private func loadChat(session: String) {
        chatSession = session
        chat = []
        let core = core
        Task.detached(priority: .utility) {
            let history = core.chatHistory(session: session, limit: 500, before: nil)
            await MainActor.run {
                guard self.chatSession == session else { return }
                let pending = self.chat
                self.chat = (history.sorted { $0.tsMs < $1.tsMs } + pending)
                    .reduce(into: [ChatMessage]()) {
                        acc, m in if !acc.contains(where: { $0.id == m.id }) {
                            acc.append(m)
                        }
                    }
            }
        }
    }

    private func receive(_ message: ChatMessage) {
        guard message.session == snapshot.currentSession || chatSession == nil else { return }
        if !chat.contains(where: { $0.id == message.id }) {
            chat.append(message)
            if chatOpen {
                core.markRead(upTo: message.id)
            } else if !message.own {
                unread += 1
            }
        }
    }

    func sendChat(_ text: String) {
        let body = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !body.isEmpty, let session = snapshot.currentSession else { return }
        let id = core.sendChat(body: body)
        // The core does not echo our own messages; show it now, receipts update it.
        chat.append(ChatMessage(
            id: id, session: session, tsMs: UInt64(Date().timeIntervalSince1970 * 1000),
            senderKey: snapshot.device.signingKey, senderName: snapshot.settings.displayName,
            kind: .text, body: body, own: true, status: snapshot.peers.isEmpty ? .sending : .sent
        ))
    }

    private func markAllRead() {
        unread = 0
        if let last = chat.last(where: { !$0.own }) {
            core.markRead(upTo: last.id)
        }
    }

    // MARK: - Files

    var transfers: [TransferInfo] {
        snapshot.transfers.filter { $0.session == snapshot.currentSession }
    }

    /// Sends a file picked in the document picker to the chosen participants. The
    /// security-scoped URL stays open until the transfer ends, so nothing is copied.
    func offerFile(_ url: URL, to recipients: [String]) {
        guard !recipients.isEmpty else { return }
        let scoped = url.startAccessingSecurityScopedResource()
        let id = core.offerFile(path: url.path, recipients: recipients)
        if scoped {
            scopedFiles[id] = url
        }
        refresh()
    }

    func respond(to offer: TransferInfo, accept: Bool) {
        core.respondFileOffer(id: offer.id, accept: accept)
        fileOffers.removeAll { $0.id == offer.id }
        refresh()
    }

    func cancel(_ transfer: TransferInfo) {
        core.cancelTransfer(id: transfer.id)
    }

    private func updateTransfer(_ t: TransferInfo) {
        if let i = snapshot.transfers.firstIndex(where: { $0.id == t.id }) {
            snapshot.transfers[i] = t
        } else {
            snapshot.transfers.append(t)
        }
        switch t.state {
        case .completed, .declined, .cancelled, .failed:
            scopedFiles.removeValue(forKey: t.id)?.stopAccessingSecurityScopedResource()
        case .offered, .accepted, .transferring, .paused:
            break
        }
    }

    // MARK: - Trust

    func trust(_ prompt: TrustPromptItem) {
        if let key = Data(hex: prompt.peerKey) {
            core.trustDevice(signingKey: key)
        }
        trustPrompts.removeAll { $0.peerKey == prompt.peerKey }
    }

    func dismiss(_ prompt: TrustPromptItem) {
        trustPrompts.removeAll { $0.peerKey == prompt.peerKey }
    }

    func untrust(_ device: TrustedDevice) {
        if let key = Data(hex: device.key) {
            core.untrustDevice(signingKey: key)
        }
    }

    // MARK: - Settings

    func setDisplayName(_ name: String) {
        let n = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !n.isEmpty, n != snapshot.settings.displayName else { return }
        core.setDisplayName(name: n)
        snapshot.settings.displayName = n
    }

    func setApm(_ cfg: ApmConfig) {
        core.setAudioProcessing(cfg: cfg)
        snapshot.settings.apm = cfg
        snapshot.settings.rawMic = false
    }

    func setRawMic(_ raw: Bool) {
        core.setRawMic(raw: raw)
        snapshot.settings.rawMic = raw
    }

    func setBitrate(_ kbps: UInt32) {
        core.setBitrateCeilingKbps(kbps: kbps)
        snapshot.settings.bitrateCeilingKbps = kbps
    }

    func setSignalingUrl(_ url: String) {
        let u = url.trimmingCharacters(in: .whitespacesAndNewlines)
        core.setSignalingUrl(url: u.isEmpty ? nil : u)
    }

    /// Writes the diagnostics report (JSON) to Documents off the main actor; returns its URL.
    func exportDiagnostics() async -> URL? {
        let core = core
        let text = await Task.detached(priority: .userInitiated) { core.exportDiagnostics() }.value
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("callxl-diagnostics-\(Int(Date().timeIntervalSince1970)).json")
        do {
            try text.write(to: url, atomically: true, encoding: .utf8)
            return url
        } catch {
            lastError = "Could not write the diagnostics report: \(error.localizedDescription)"
            return nil
        }
    }
}

// MARK: - CallKit / audio session callbacks

extension AppModel: CallCommands {
    func performJoin(_ target: JoinTarget) {
        audio.configureForCall()
        switch target {
        case let .create(name): _ = core.createSession(name: name)
        case let .existing(id): core.joinSession(id: id)
        }
        refresh()
    }

    func performLeave() {
        core.leaveSession()
        refresh()
    }

    func performSetMuted(_ muted: Bool) {
        userMuted = muted
        core.setMuted(muted: muted || held)
        snapshot.muted = muted || held
    }

    func performHold(_ onHold: Bool) {
        held = onHold
        core.setMuted(muted: userMuted || onHold)
        snapshot.muted = userMuted || onHold
    }

    func audioSessionActivated(_ session: AVAudioSession) {
        audio.callKitActivated(session)
    }

    func audioSessionDeactivated(_ session: AVAudioSession) {
        audio.callKitDeactivated(session)
    }

    func callKitUnavailable(_ target: JoinTarget) {
        Self.log.info("CallKit unavailable; joining without the system call UI")
        performJoin(target)
        audio.activateWithoutCallKit()
    }
}
