import SwiftUI

/// §9 Home: open sessions (LAN glyph), "+ New session", trusted devices online/offline.
struct HomeView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    @State private var creating = false
    @State private var newName = ""
    @State private var launchMs: Double?

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: Tokens.Space.xl) {
                newSessionButton
                sessions
                trusted
                if let launchMs, LaunchMode.reportLaunch {
                    Text(String(format: "%.0f", launchMs))
                        .font(.caption2)
                        .foregroundStyle(theme.textMuted)
                        .accessibilityIdentifier("home.launchMs")
                    Text(LaunchClock.breakdown)
                        .font(.caption2)
                        .foregroundStyle(theme.textMuted)
                        .accessibilityIdentifier("home.launchBreakdown")
                }
            }
            .padding(Tokens.Space.lg)
        }
        .background(theme.bg.ignoresSafeArea())
        .navigationTitle("CallXL")
        .toolbar {
            ToolbarItem(placement: .navigationBarLeading) {
                NavigationLink {
                    DiagnosticsView()
                } label: {
                    Image(systemName: "waveform.path.ecg")
                }
                .accessibilityLabel("Diagnostics")
                .accessibilityIdentifier("home.diagnostics")
            }
            ToolbarItem(placement: .navigationBarTrailing) {
                NavigationLink {
                    SettingsView()
                } label: {
                    Image(systemName: "gearshape")
                }
                .accessibilityLabel("Settings")
                .accessibilityIdentifier("home.settings")
            }
        }
        .sheet(isPresented: $creating) {
            newSessionSheet.themed()
        }
        .onAppear {
            if launchMs == nil {
                LaunchClock.mark("home.appear")
                let ms = LaunchClock.msSinceProcessStart()
                launchMs = ms
                if LaunchMode.reportLaunch {
                    LaunchClock.writeReport(ms) // read by CI after `simctl launch`
                }
                AppModel.log.info("launch-to-home-ms: \(ms, privacy: .public)")
            }
        }
    }

    private var newSessionButton: some View {
        Button {
            newName = ""
            creating = true
        } label: {
            Label("New session", systemImage: "plus")
                .font(.token(Tokens.Typography.subtitle))
                .frame(maxWidth: .infinity, minHeight: Tokens.Size.tapTarget + Tokens.Space.sm)
        }
        .buttonStyle(.borderedProminent)
        .buttonBorderShape(.roundedRectangle(radius: Tokens.Radius.lg))
        .disabled(!model.started)
        .accessibilityIdentifier("home.newSession")
    }

    private var sessions: some View {
        VStack(alignment: .leading, spacing: Tokens.Space.sm) {
            SectionTitle("Open sessions")
            if model.snapshot.sessions.isEmpty {
                Text(model.started ? "No open sessions yet. Start one, or wait for someone on your Wi-Fi." : "Looking for sessions…")
                    .font(.token(Tokens.Typography.body))
                    .foregroundStyle(theme.textMuted)
                    .card(theme)
            }
            ForEach(model.snapshot.sessions, id: \.id) { s in
                Button {
                    model.join(s)
                } label: {
                    SessionRow(session: s)
                }
                .buttonStyle(.plain)
                .accessibilityIdentifier("home.session.\(s.id)")
            }
        }
    }

    private var trusted: some View {
        VStack(alignment: .leading, spacing: Tokens.Space.sm) {
            SectionTitle("Trusted devices")
            if model.snapshot.trustedDevices.isEmpty {
                Text("Devices you trust appear here.")
                    .font(.token(Tokens.Typography.body))
                    .foregroundStyle(theme.textMuted)
                    .card(theme)
            }
            ForEach(model.snapshot.trustedDevices, id: \.key) { d in
                HStack(spacing: Tokens.Space.md) {
                    Circle()
                        .fill(d.online ? theme.success : theme.textMuted.opacity(0.4))
                        .frame(width: 10, height: 10)
                    Text(d.displayName)
                        .font(.token(Tokens.Typography.body))
                        .foregroundStyle(theme.text)
                    Spacer()
                    Text(d.online ? "Online" : "Offline")
                        .font(.token(Tokens.Typography.caption))
                        .foregroundStyle(theme.textMuted)
                }
                .card(theme)
                .accessibilityElement(children: .combine)
            }
        }
    }

    private var newSessionSheet: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("Name (optional)", text: $newName)
                        .textInputAutocapitalization(.sentences)
                        .accessibilityIdentifier("newSession.name")
                } footer: {
                    Text("Everyone on your Wi-Fi (and with this app's cloud access) sees the session and can join.")
                }
            }
            .navigationTitle("New session")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { creating = false }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Start") {
                        creating = false
                        model.createSession(name: newName)
                    }
                    .accessibilityIdentifier("newSession.create")
                }
            }
        }
        .presentationDetents([.medium])
    }
}

struct SectionTitle: View {
    @Environment(\.theme) private var theme
    let text: String

    init(_ text: String) {
        self.text = text
    }

    var body: some View {
        Text(text.uppercased())
            .font(.token(Tokens.Typography.caption))
            .foregroundStyle(theme.textMuted)
            .padding(.leading, Tokens.Space.xs)
    }
}

struct SessionRow: View {
    @Environment(\.theme) private var theme
    let session: SessionSummary

    var body: some View {
        HStack(spacing: Tokens.Space.md) {
            Image(systemName: session.lan ? "wifi" : "cloud")
                .foregroundStyle(session.lan ? theme.accent : theme.textMuted)
                .frame(width: 28)
                .accessibilityLabel(session.lan ? "On your Wi-Fi" : "Over the internet")
            VStack(alignment: .leading, spacing: Tokens.Space.xxs) {
                Text(session.name ?? "Untitled session")
                    .font(.token(Tokens.Typography.subtitle))
                    .foregroundStyle(theme.text)
                Text(session.members.map(\.displayName).joined(separator: ", "))
                    .font(.token(Tokens.Typography.caption))
                    .foregroundStyle(theme.textMuted)
                    .lineLimit(1)
            }
            Spacer()
            Text("\(session.members.count)")
                .font(.token(Tokens.Typography.bodyStrong))
                .foregroundStyle(theme.textMuted)
            Image(systemName: "chevron.right")
                .foregroundStyle(theme.textMuted)
        }
        .frame(minHeight: Tokens.Size.tapTarget)
        .card(theme)
        .contentShape(Rectangle())
    }
}
