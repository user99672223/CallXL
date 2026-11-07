import Foundation

/// In-app stand-in for the Rust core, used only when launched with `-uiTestFakeCore`
/// (UI smoke tests drive every screen without audio, network or permissions).
final class FakeCore: CallXlProtocol, @unchecked Sendable {
    private let lock = NSLock()
    private var state: Snapshot
    private var listeners: [EventListener] = []
    private var messages: [ChatMessage] = []
    private var timer: DispatchSourceTimer?
    private var tick: UInt64 = 0

    static let peerKey = String(repeating: "ab", count: 32)

    init() {
        let me = DeviceCard(displayName: "Test iPhone", signingKey: String(repeating: "11", count: 32), agreementKey: String(repeating: "22", count: 32))
        let settings = Settings(
            displayName: me.displayName, signalingUrl: nil, effectiveSignalingUrl: "wss://callxl.<account>.workers.dev/ws",
            downloadDir: NSTemporaryDirectory(), bitrateCeilingKbps: 510,
            apm: ApmConfig(aec: true, noiseSuppression: true, nsLevel: .moderate, highPass: true, agc: true),
            rawMic: false, inputDevice: nil, outputDevice: nil
        )
        state = Snapshot(
            device: me, sessions: [], currentSession: nil, callState: .idle, peers: [], muted: false,
            trustedDevices: [TrustedDevice(key: Self.peerKey, displayName: "Desk PC", online: true, pinnedMs: 0)],
            transfers: [], settings: settings, cloudConnected: false
        )
    }

    private func mutate(_ f: (inout Snapshot) -> Void) {
        lock.lock()
        f(&state)
        lock.unlock()
    }

    private func emit(_ e: Event) {
        lock.lock()
        let ls = listeners
        lock.unlock()
        for l in ls {
            l.onEvent(event: e)
        }
    }

    private var peer: PeerInfo {
        PeerInfo(
            key: Self.peerKey, displayName: "Desk PC", trusted: true, connected: true, speaking: tick % 2 == 0, lan: true,
            path: PathInfo(
                kind: .lan, localAddr: "192.168.1.20:50000", localFamily: .v4, remoteAddr: "192.168.1.10:50000",
                remoteFamily: .v4, localCandidateType: "host", remoteCandidateType: "host", switches: 0
            )
        )
    }

    private func stats() -> DiagnosticsSnapshot {
        let p = PeerStats(
            peerKey: Self.peerKey, displayName: "Desk PC", path: peer.path, rttMs: 3, jitterMs: 1, lossPct: 0, remoteLossPct: 0,
            sendKbps: 548, recvKbps: 547, opusFrameMs: 10, jitterBufferMs: 20, concealmentEvents: 0, pairSwitches: 0,
            opusBitrateKbps: 510, opusPayloadKbps: 480, fecEnabled: true, dtxEnabled: false, codec: "opus", clockRate: 48000,
            channels: 1, opusComplexity: 10, availableOutgoingKbps: 600, audioLevel: 0.2, fileRateLimitKbps: 0
        )
        let g = GlobalStats(
            uplinkKbps: 548, downlinkKbps: 547, mediaThreadCpuPct: 2, signalingTransport: .lan, signalingLatencyMs: 1,
            turnBytesMonth: 0, apm: state.settings.apm, frameAdaptationMechanism: "fake", anaFrameMs: 10, anaSwitches: 0,
            anaUplinkLossPct: 0, rejectedSignaling: 0, glareEvents: 0, aecErlDb: -1, aecErleDb: -1, aecDelayMs: -1, droppedMediaEvents: 0
        )
        let series = ["rtt_ms:Desk PC", "uplink_kbps"].map { m in
            Sparkline(metric: m, values: (0 ..< 60).map { i in 500 + 10 * sin(Double(i) / 5) })
        }
        return DiagnosticsSnapshot(tsMs: UInt64(Date().timeIntervalSince1970 * 1000), peers: [p], global: g, series: series)
    }

    // MARK: CallXlProtocol

    func start() throws {
        let session = SessionSummary(
            id: "lan-kitchen", name: "Kitchen", members: [SessionMember(key: Self.peerKey, displayName: "Desk PC")],
            lan: true, cloud: false, joined: false
        )
        mutate { $0.sessions = [session] }
        emit(.sessionsChanged(sessions: [session]))
        let t = DispatchSource.makeTimerSource(queue: .global())
        t.schedule(deadline: .now() + 1, repeating: 1)
        t.setEventHandler { [weak self] in self?.onTick() }
        t.resume()
        timer = t
    }

    private func onTick() {
        lock.lock()
        tick += 1
        let inCall = state.currentSession != nil
        lock.unlock()
        guard inCall else { return }
        mutate { $0.peers = [self.peer] }
        emit(.speakingChanged(peerKey: Self.peerKey, speaking: tick % 2 == 0))
        emit(.statsTick(snapshot: stats()))
    }

    func stop() {
        timer?.cancel()
        timer = nil
    }

    func subscribe(listener: EventListener) {
        lock.lock()
        listeners.append(listener)
        lock.unlock()
    }

    func snapshot() -> Snapshot {
        lock.lock()
        defer { lock.unlock() }
        return state
    }

    func settings() -> Settings {
        snapshot().settings
    }

    func deviceCard() -> DeviceCard {
        snapshot().device
    }

    func diagnostics() -> DiagnosticsSnapshot {
        stats()
    }

    func listAudioDevices() -> [AudioDevice] {
        []
    }

    func createSession(name: String?) -> String {
        let id = "fake-\(UUID().uuidString.prefix(8))"
        let s = SessionSummary(
            id: id, name: name, members: [SessionMember(key: state.device.signingKey, displayName: state.device.displayName)],
            lan: true, cloud: false, joined: true
        )
        mutate { $0.sessions.append(s) }
        joinSession(id: id)
        return id
    }

    func joinSession(id: String) {
        mutate {
            $0.currentSession = id
            $0.callState = .inCall
            $0.peers = [self.peer]
        }
        emit(.callStateChanged(state: .joining))
        emit(.callStateChanged(state: .inCall))
        emit(.peerJoined(peer: peer))
    }

    func leaveSession() {
        mutate {
            $0.currentSession = nil
            $0.callState = .idle
            $0.peers = []
        }
        emit(.callStateChanged(state: .idle))
    }

    func setMuted(muted: Bool) {
        mutate { $0.muted = muted }
    }

    func setDisplayName(name: String) {
        mutate { $0.settings.displayName = name }
    }

    func trustDevice(signingKey _: Data) {}
    func untrustDevice(signingKey _: Data) {}
    func setInputDevice(id _: String?) {}
    func setOutputDevice(id _: String?) {}
    func setAudioProcessing(cfg: ApmConfig) {
        mutate { $0.settings.apm = cfg }
    }

    func setRawMic(raw: Bool) {
        mutate { $0.settings.rawMic = raw }
    }

    func setBitrateCeilingKbps(kbps: UInt32) {
        mutate { $0.settings.bitrateCeilingKbps = kbps }
    }

    func setSignalingUrl(url: String?) {
        mutate { $0.settings.signalingUrl = url }
    }

    func setDownloadDir(path: String) {
        mutate { $0.settings.downloadDir = path }
    }

    func sendChat(body: String) -> String {
        let id = UUID().uuidString
        // The far end answers, so the thread shows both sides.
        let reply = ChatMessage(
            id: UUID().uuidString, session: state.currentSession ?? "", tsMs: UInt64(Date().timeIntervalSince1970 * 1000) + 1,
            senderKey: Self.peerKey, senderName: "Desk PC", kind: .text, body: "re: \(body)", own: false, status: .delivered
        )
        DispatchQueue.global().asyncAfter(deadline: .now() + 0.3) { [weak self] in
            self?.emit(.receiptUpdated(messageId: id, status: .delivered, peerKey: Self.peerKey))
            self?.emit(.chatReceived(message: reply))
        }
        return id
    }

    func markRead(upTo _: String) {}
    func chatHistory(session _: String, limit _: UInt32, before _: String?) -> [ChatMessage] {
        messages
    }

    func offerFile(path _: String, recipients _: [String]) -> String {
        UUID().uuidString
    }

    func respondFileOffer(id _: String, accept _: Bool) {}
    func cancelTransfer(id _: String) {}

    func exportDiagnostics() -> String {
        // Same top-level shape as the core's schema v1 export.
        #"{"schema":1,"app":"CallXL","call":null,"peers":[],"global":{},"series":[],"log_tail":[],"fake":true}"#
    }
}
