import Charts
import SwiftUI

/// §7 Diagnostics: per-peer path/RTT/jitter/loss/bitrate/frame size/jitter buffer/
/// concealment/switches, global totals, 60 s sparklines, "Export report". Values come
/// from the core's 1 Hz `StatsTick` (polled off the UI thread by the core).
struct DiagnosticsView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    @State private var exported: URL?
    @State private var exporting = false

    private var snap: DiagnosticsSnapshot? {
        model.diagnostics
    }

    var body: some View {
        List {
            Section {
                Button {
                    exporting = true
                    Task {
                        exported = await model.exportDiagnostics()
                        exporting = false
                    }
                } label: {
                    HStack {
                        Label("Export report", systemImage: "square.and.arrow.up")
                        if exporting {
                            Spacer(); ProgressView()
                        }
                    }
                }
                .accessibilityIdentifier("diag.export")
                if let exported {
                    ShareLink(item: exported) {
                        Label(exported.lastPathComponent, systemImage: "doc.text")
                    }
                    .accessibilityIdentifier("diag.exported")
                }
            }

            if let g = snap?.global {
                Section("Global") {
                    Group {
                        row("Uplink", kbps(g.uplinkKbps))
                        row("Downlink", kbps(g.downlinkKbps))
                        row("Media thread CPU", String(format: "%.1f %%", g.mediaThreadCpuPct))
                        row("Signaling", "\(transport(g.signalingTransport)) · \(ms(g.signalingLatencyMs))")
                        row("TURN this month", ByteCountFormatter.string(fromByteCount: Int64(g.turnBytesMonth), countStyle: .binary))
                        row("Frame size (adaptor)", "\(g.anaFrameMs) ms · \(g.anaSwitches) switches · \(g.frameAdaptationMechanism)")
                        row("Uplink loss (5 s)", String(format: "%.1f %%", g.anaUplinkLossPct))
                        row("APM", apm(g.apm))
                    }
                    if g.aecErleDb >= 0 {
                        row("AEC", String(format: "ERL %.1f dB · ERLE %.1f dB · delay %d ms", g.aecErlDb, g.aecErleDb, g.aecDelayMs))
                    }
                    row("Rejected signaling", "\(g.rejectedSignaling)")
                    row("Glare events", "\(g.glareEvents)")
                    row("Dropped media events", "\(g.droppedMediaEvents)")
                }
            } else {
                Section {
                    Text("Statistics appear once the core is running.")
                        .foregroundStyle(theme.textMuted)
                }
            }

            ForEach(snap?.peers ?? [], id: \.peerKey) { p in
                Section(p.displayName) {
                    Group {
                        row("Path", path(p.path))
                        if let path = p.path {
                            row("Local", "\(path.localAddr) (\(fam(path.localFamily)), \(path.localCandidateType))")
                            row("Remote", "\(path.remoteAddr) (\(fam(path.remoteFamily)), \(path.remoteCandidateType))")
                        }
                        row("RTT", ms(p.rttMs))
                        row("Jitter", ms(p.jitterMs))
                        row("Loss in / out", String(format: "%.1f %% / %.1f %%", p.lossPct, p.remoteLossPct))
                        row("Send / receive", "\(kbps(p.sendKbps)) / \(kbps(p.recvKbps))")
                    }
                    row(
                        "Opus",
                        "\(p.opusFrameMs) ms · \(kbps(p.opusBitrateKbps)) target · FEC \(p.fecEnabled ? "on" : "off") · DTX \(p.dtxEnabled ? "on" : "off")"
                    )
                    row("Jitter buffer", ms(p.jitterBufferMs))
                    row("Concealment events", "\(p.concealmentEvents)")
                    row("Pair switches", "\(p.pairSwitches)")
                    row("Available outgoing", kbps(p.availableOutgoingKbps))
                    if p.fileRateLimitKbps > 0 {
                        row("File throttle", kbps(p.fileRateLimitKbps))
                    }
                }
            }

            if let series = snap?.series, !series.isEmpty {
                Section("Last 60 s") {
                    ForEach(series, id: \.metric) { s in
                        SparklineRow(series: s)
                    }
                }
            }
        }
        .navigationTitle("Diagnostics")
        .scrollContentBackground(.hidden)
        .background(theme.bg.ignoresSafeArea())
    }

    private func row(_ k: String, _ v: String) -> some View {
        HStack(alignment: .firstTextBaseline) {
            Text(k).foregroundStyle(theme.textMuted)
            Spacer(minLength: Tokens.Space.md)
            Text(v).foregroundStyle(theme.text).multilineTextAlignment(.trailing)
        }
        .font(.token(Tokens.Typography.caption))
    }

    private func kbps(_ v: Double) -> String {
        String(format: "%.0f kbps", v)
    }

    private func ms(_ v: Double) -> String {
        String(format: "%.0f ms", v)
    }

    private func fam(_ f: AddrFamily) -> String {
        f == .v6 ? "IPv6" : "IPv4"
    }

    private func transport(_ t: SignalingTransport) -> String {
        switch t {
        case .lan: "LAN"
        case .cloud: "Cloud"
        case .none: "None"
        }
    }

    private func path(_ p: PathInfo?) -> String {
        guard let p else { return "—" }
        switch p.kind {
        case .lan: return "LAN"
        case .direct: return "Direct"
        case .relayed: return "Relayed"
        }
    }

    private func apm(_ a: ApmConfig) -> String {
        [a.aec ? "AEC" : nil, a.noiseSuppression ? "NS" : nil, a.highPass ? "HPF" : nil, a.agc ? "AGC" : nil]
            .compactMap { $0 }.joined(separator: " · ").ifEmpty("Raw mic")
    }
}

private extension String {
    func ifEmpty(_ fallback: String) -> String {
        isEmpty ? fallback : self
    }
}

struct SparklineRow: View {
    @Environment(\.theme) private var theme
    let series: Sparkline

    var body: some View {
        VStack(alignment: .leading, spacing: Tokens.Space.xs) {
            HStack {
                Text(series.metric)
                    .font(.token(Tokens.Typography.caption))
                    .foregroundStyle(theme.textMuted)
                Spacer()
                Text(series.values.last.map { String(format: "%.1f", $0) } ?? "—")
                    .font(.token(Tokens.Typography.caption))
                    .foregroundStyle(theme.text)
            }
            Chart {
                ForEach(Array(series.values.enumerated()), id: \.offset) { point in
                    LineMark(x: .value("t", point.offset), y: .value("v", point.element))
                        .foregroundStyle(theme.accent)
                        .interpolationMethod(.monotone)
                }
            }
            .chartXAxis(.hidden)
            .chartYAxis(.hidden)
            .frame(height: 36)
        }
        .accessibilityElement(children: .combine)
    }
}
