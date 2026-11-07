import AVKit
import SwiftUI
import UniformTypeIdentifiers

/// §9 In call: participant tiles (speaking glow, path badge), mute, audio route picker,
/// slide-in chat drawer, file send (document picker → recipient picker).
struct CallView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    @State private var chatOpen = false
    @State private var pickingFile = false
    @State private var pickedFile: URL?

    private let columns = [GridItem(.adaptive(minimum: 150), spacing: Tokens.Space.md)]

    var body: some View {
        ZStack(alignment: .trailing) {
            VStack(spacing: Tokens.Space.lg) {
                statusBanner
                ScrollView {
                    LazyVGrid(columns: columns, spacing: Tokens.Space.md) {
                        ParticipantTile(
                            name: "\(model.snapshot.settings.displayName) (you)",
                            speaking: false, muted: model.muted, path: nil, trusted: true
                        )
                        ForEach(model.snapshot.peers, id: \.key) { p in
                            ParticipantTile(
                                name: p.displayName, speaking: model.speaking.contains(p.key),
                                muted: false, path: p.path, trusted: p.trusted
                            )
                        }
                    }
                    .padding(.horizontal, Tokens.Space.lg)
                    if !model.transfers.isEmpty {
                        VStack(spacing: Tokens.Space.sm) {
                            ForEach(model.transfers, id: \.id) { FileCard(transfer: $0) }
                        }
                        .padding(Tokens.Space.lg)
                    }
                }
                controls
            }
            .background(theme.bg.ignoresSafeArea())

            if chatOpen {
                theme.bg.opacity(0.4)
                    .ignoresSafeArea()
                    .onTapGesture { setChat(false) }
                    .transition(.opacity)
                ChatView(onClose: { setChat(false) })
                    .frame(maxWidth: min(Tokens.Size.drawer, UIScreen.main.bounds.width * 0.92))
                    .background(theme.surface.ignoresSafeArea())
                    .transition(.move(edge: .trailing))
            }
        }
        .navigationTitle(model.currentSession?.name ?? "Session")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .navigationBarLeading) {
                NavigationLink {
                    DiagnosticsView()
                } label: {
                    Image(systemName: "waveform.path.ecg")
                }
                .accessibilityLabel("Diagnostics")
                .accessibilityIdentifier("call.diagnostics")
            }
            ToolbarItem(placement: .navigationBarTrailing) {
                Button {
                    setChat(!chatOpen)
                } label: {
                    Image(systemName: model.unread > 0 ? "bubble.left.and.bubble.right.fill" : "bubble.left.and.bubble.right")
                        .overlay(alignment: .topTrailing) {
                            if model.unread > 0 {
                                Circle().fill(theme.accent).frame(width: 8, height: 8).offset(x: 4, y: -4)
                            }
                        }
                }
                .accessibilityLabel("Chat")
                .accessibilityIdentifier("call.chat")
            }
        }
        .fileImporter(isPresented: $pickingFile, allowedContentTypes: [.item]) { result in
            if case let .success(url) = result {
                pickedFile = url
            }
        }
        .sheet(item: Binding(get: { pickedFile.map(PickedFile.init) }, set: {
            if $0 == nil {
                pickedFile = nil
            }
        })) { f in
            RecipientPicker(file: f.url) { recipients in
                model.offerFile(f.url, to: recipients)
                pickedFile = nil
            }
            .themed()
        }
    }

    private func setChat(_ open: Bool) {
        withAnimation(.easeOut(duration: Tokens.Motion.normal)) {
            chatOpen = open
        }
        model.chatOpen = open
    }

    @ViewBuilder
    private var statusBanner: some View {
        let text: String? = switch model.snapshot.callState {
        case .joining: "Joining…"
        case let .reconnecting(reason): reason.isEmpty ? "Reconnecting…" : "Reconnecting: \(reason)"
        default: model.interrupted ? "Paused by another call" : nil
        }
        if let text {
            Text(text)
                .font(.token(Tokens.Typography.caption))
                .foregroundStyle(theme.text)
                .padding(.horizontal, Tokens.Space.md)
                .padding(.vertical, Tokens.Space.xs)
                .background(theme.surfaceRaised, in: Capsule())
                .accessibilityIdentifier("call.status")
        }
    }

    private var controls: some View {
        HStack(spacing: Tokens.Space.xl) {
            RoundButton(
                systemImage: model.muted ? "mic.slash.fill" : "mic.fill",
                label: model.muted ? "Unmute" : "Mute",
                tint: model.muted ? theme.danger : theme.surfaceRaised
            ) { model.toggleMute() }
                .accessibilityIdentifier("call.mute")
            RoutePicker()
                .frame(width: 56, height: 56)
                .background(theme.surfaceRaised, in: Circle())
                .accessibilityLabel("Audio output")
                .accessibilityIdentifier("call.route")
            RoundButton(systemImage: "doc.badge.plus", label: "Send file", tint: theme.surfaceRaised) {
                pickingFile = true
            }
            .disabled(model.snapshot.peers.isEmpty)
            .accessibilityIdentifier("call.sendFile")
            RoundButton(systemImage: "phone.down.fill", label: "Leave", tint: theme.danger) {
                model.leave()
            }
            .accessibilityIdentifier("call.leave")
        }
        .padding(.bottom, Tokens.Space.lg)
    }
}

private struct PickedFile: Identifiable {
    let url: URL
    var id: String {
        url.absoluteString
    }
}

struct RoundButton: View {
    @Environment(\.theme) private var theme
    let systemImage: String
    let label: String
    let tint: Color
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: systemImage)
                .font(.system(size: 22, weight: .semibold))
                .foregroundStyle(theme.text)
                .frame(width: 56, height: 56)
                .background(tint, in: Circle())
        }
        .accessibilityLabel(label)
    }
}

/// `AVRoutePickerView`: speaker / receiver / Bluetooth (the iOS ADM follows the route).
struct RoutePicker: UIViewRepresentable {
    @Environment(\.theme) private var theme

    func makeUIView(context _: Context) -> AVRoutePickerView {
        let v = AVRoutePickerView()
        v.prioritizesVideoDevices = false
        return v
    }

    func updateUIView(_ v: AVRoutePickerView, context _: Context) {
        v.tintColor = UIColor(theme.text)
        v.activeTintColor = UIColor(theme.accent)
    }
}

/// Participant tile; the speaking glow animates on the render server (opacity/shadow
/// via `withAnimation`), the main actor only flips a Bool.
struct ParticipantTile: View {
    @Environment(\.theme) private var theme
    let name: String
    let speaking: Bool
    let muted: Bool
    let path: PathInfo?
    let trusted: Bool

    var body: some View {
        VStack(spacing: Tokens.Space.sm) {
            ZStack {
                Circle()
                    .fill(theme.surfaceRaised)
                    .frame(width: 72, height: 72)
                Text(initials)
                    .font(.token(Tokens.Typography.title))
                    .foregroundStyle(theme.text)
            }
            .overlay(
                Circle()
                    .stroke(theme.speakingGlow, lineWidth: 3)
                    .opacity(speaking ? 1 : 0)
            )
            .shadow(color: theme.speakingGlow.opacity(speaking ? 1 : 0), radius: speaking ? 14 : 0)
            .animation(.easeInOut(duration: Tokens.Motion.glow), value: speaking)
            HStack(spacing: Tokens.Space.xs) {
                if muted {
                    Image(systemName: "mic.slash.fill").foregroundStyle(theme.danger)
                }
                Text(name)
                    .font(.token(Tokens.Typography.bodyStrong))
                    .foregroundStyle(theme.text)
                    .lineLimit(1)
            }
            if let path {
                PathBadge(path: path)
            }
        }
        .frame(maxWidth: .infinity, minHeight: Tokens.Size.tile)
        .background(theme.surface, in: RoundedRectangle(cornerRadius: Tokens.Radius.xl, style: .continuous))
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(name)\(speaking ? ", speaking" : "")\(muted ? ", muted" : "")")
    }

    private var initials: String {
        let parts = name.split(separator: " ").prefix(2)
        return parts.compactMap { $0.first.map(String.init) }.joined().uppercased()
    }
}

struct PathBadge: View {
    @Environment(\.theme) private var theme
    let path: PathInfo

    var body: some View {
        Text("\(label) · \(path.remoteFamily == .v6 ? "IPv6" : "IPv4")")
            .font(.token(Tokens.Typography.caption))
            .foregroundStyle(path.kind == .relayed ? theme.textMuted : theme.accent)
            .padding(.horizontal, Tokens.Space.sm)
            .padding(.vertical, Tokens.Space.xxs)
            .background(theme.surfaceRaised, in: Capsule())
    }

    private var label: String {
        switch path.kind {
        case .lan: "LAN"
        case .direct: "Direct"
        case .relayed: "Relayed"
        }
    }
}

/// Choose which participants receive a file (§6: any subset).
struct RecipientPicker: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.dismiss) private var dismiss
    let file: URL
    let send: ([String]) -> Void
    @State private var selected: Set<String> = []

    var body: some View {
        NavigationStack {
            List(model.snapshot.peers, id: \.key, selection: $selected) { p in
                Text(p.displayName)
            }
            .environment(\.editMode, .constant(.active))
            .navigationTitle(file.lastPathComponent)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Send") { send(Array(selected)) }
                        .disabled(selected.isEmpty)
                        .accessibilityIdentifier("file.send")
                }
            }
            .onAppear { selected = Set(model.snapshot.peers.map(\.key)) }
        }
    }
}
