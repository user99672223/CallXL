import SwiftUI

/// §9 Chat: the session's thread with receipts, plus file cards (progress, Share, Files).
struct ChatView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    @State private var draft = ""
    @FocusState private var focused: Bool
    var onClose: (() -> Void)?

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text("Chat")
                    .font(.token(Tokens.Typography.title))
                    .foregroundStyle(theme.text)
                Spacer()
                if let onClose {
                    Button(action: onClose) {
                        Image(systemName: "xmark")
                            .frame(width: Tokens.Size.tapTarget, height: Tokens.Size.tapTarget)
                    }
                    .accessibilityLabel("Close chat")
                    .accessibilityIdentifier("chat.close")
                }
            }
            .padding(.horizontal, Tokens.Space.lg)
            .padding(.top, Tokens.Space.md)

            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: Tokens.Space.sm) {
                        ForEach(model.chat, id: \.id) { m in
                            MessageBubble(message: m).id(m.id)
                        }
                        ForEach(model.transfers, id: \.id) { FileCard(transfer: $0) }
                    }
                    .padding(Tokens.Space.lg)
                }
                .onChange(of: model.chat.count) { _ in
                    if let last = model.chat.last {
                        withAnimation(.easeOut(duration: Tokens.Motion.fast)) { proxy.scrollTo(last.id, anchor: .bottom) }
                    }
                }
            }

            HStack(spacing: Tokens.Space.sm) {
                TextField("Message", text: $draft, axis: .vertical)
                    .lineLimit(1 ... 4)
                    .focused($focused)
                    .padding(.horizontal, Tokens.Space.md)
                    .padding(.vertical, Tokens.Space.sm)
                    .background(theme.surfaceRaised, in: RoundedRectangle(cornerRadius: Tokens.Radius.lg, style: .continuous))
                    .submitLabel(.send)
                    .onSubmit(send)
                    .accessibilityIdentifier("chat.input")
                Button(action: send) {
                    Image(systemName: "arrow.up.circle.fill")
                        .font(.system(size: 30))
                }
                .disabled(draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                .accessibilityLabel("Send")
                .accessibilityIdentifier("chat.send")
            }
            .padding(Tokens.Space.md)
        }
    }

    private func send() {
        model.sendChat(draft)
        draft = ""
    }
}

struct MessageBubble: View {
    @Environment(\.theme) private var theme
    let message: ChatMessage

    var body: some View {
        HStack {
            if message.own {
                Spacer(minLength: Tokens.Space.xxl)
            }
            VStack(alignment: message.own ? .trailing : .leading, spacing: Tokens.Space.xxs) {
                if !message.own {
                    Text(message.senderName)
                        .font(.token(Tokens.Typography.caption))
                        .foregroundStyle(theme.textMuted)
                }
                Text(message.body)
                    .font(.token(Tokens.Typography.body))
                    .foregroundStyle(message.own ? theme.onAccent : theme.text)
                    .padding(.horizontal, Tokens.Space.md)
                    .padding(.vertical, Tokens.Space.sm)
                    .background(
                        message.own ? theme.accent : theme.surfaceRaised,
                        in: RoundedRectangle(cornerRadius: Tokens.Radius.lg, style: .continuous)
                    )
                if message.own {
                    Text(receipt)
                        .font(.token(Tokens.Typography.caption))
                        .foregroundStyle(theme.textMuted)
                }
            }
            if !message.own {
                Spacer(minLength: Tokens.Space.xxl)
            }
        }
        .accessibilityElement(children: .combine)
    }

    private var receipt: String {
        switch message.status {
        case .sending: "Sending"
        case .sent: "Sent"
        case .delivered: "Delivered"
        case .read: "Read"
        }
    }
}

/// A file transfer with progress; received files can be shared or shown in Files.
struct FileCard: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    @Environment(\.openURL) private var openURL
    let transfer: TransferInfo

    var body: some View {
        VStack(alignment: .leading, spacing: Tokens.Space.sm) {
            HStack {
                Image(systemName: transfer.direction == .incoming ? "arrow.down.doc" : "arrow.up.doc")
                    .foregroundStyle(theme.accent)
                VStack(alignment: .leading, spacing: Tokens.Space.xxs) {
                    Text(transfer.name)
                        .font(.token(Tokens.Typography.bodyStrong))
                        .foregroundStyle(theme.text)
                        .lineLimit(1)
                    Text("\(ByteCountFormatter.string(fromByteCount: Int64(transfer.size), countStyle: .file)) · \(stateText)")
                        .font(.token(Tokens.Typography.caption))
                        .foregroundStyle(theme.textMuted)
                }
                Spacer()
            }
            if isActive {
                ProgressView(value: transfer.size == 0 ? 0 : Double(transfer.bytesDone) / Double(transfer.size))
            }
            HStack(spacing: Tokens.Space.md) {
                if isDone, transfer.direction == .incoming {
                    let url = URL(fileURLWithPath: transfer.path)
                    ShareLink(item: url) {
                        Label("Share", systemImage: "square.and.arrow.up")
                    }
                    Button {
                        if let files = FilesApp.url(for: url) {
                            openURL(files)
                        }
                    } label: {
                        Label("Open in Files", systemImage: "folder")
                    }
                }
                if isActive {
                    Button(role: .destructive) {
                        model.cancel(transfer)
                    } label: {
                        Label("Cancel", systemImage: "xmark.circle")
                    }
                }
            }
            .font(.token(Tokens.Typography.caption))
        }
        .card(theme)
    }

    private var isActive: Bool {
        switch transfer.state {
        case .offered, .accepted, .transferring, .paused: true
        default: false
        }
    }

    private var isDone: Bool {
        if case .completed = transfer.state {
            return true
        }
        return false
    }

    private var stateText: String {
        switch transfer.state {
        case .offered: transfer.direction == .outgoing ? "Waiting for them to accept" : "Offered"
        case .accepted: "Accepted"
        case .transferring: "\(Int(Double(transfer.bytesDone) / Double(max(transfer.size, 1)) * 100)) %"
        case .paused: "Paused — resumes when reconnected"
        case .completed: transfer.direction == .incoming ? "Saved to Files › CallXL" : "Sent"
        case .declined: "Declined"
        case .cancelled: "Cancelled"
        case let .failed(reason): "Failed: \(reason)"
        }
    }
}

enum FilesApp {
    /// `shareddocuments://` opens the Files app at a path inside our Documents.
    static func url(for file: URL) -> URL? {
        var c = URLComponents(url: file.deletingLastPathComponent(), resolvingAgainstBaseURL: false)
        c?.scheme = "shareddocuments"
        return c?.url
    }
}

/// First contact or key change (§5): a 6-word phrase and one button.
struct TrustPromptView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    let prompt: TrustPromptItem

    var body: some View {
        VStack(spacing: Tokens.Space.xl) {
            Text(prompt.keyChanged ? "Key changed — trust again?" : prompt.displayName)
                .font(.token(Tokens.Typography.title))
                .foregroundStyle(theme.text)
                .multilineTextAlignment(.center)
            if prompt.keyChanged {
                Text("\(prompt.displayName) has a new key (reinstall or new phone).")
                    .font(.token(Tokens.Typography.body))
                    .foregroundStyle(theme.textMuted)
                    .multilineTextAlignment(.center)
            }
            Text(prompt.phrase)
                .font(.system(.title3, design: .monospaced))
                .foregroundStyle(theme.accent)
                .multilineTextAlignment(.center)
                .accessibilityIdentifier("trust.phrase")
            Text("The same words appear on their screen.")
                .font(.token(Tokens.Typography.caption))
                .foregroundStyle(theme.textMuted)
            Button {
                model.trust(prompt)
            } label: {
                Text(prompt.keyChanged ? "Trust again" : "Trust this device")
                    .frame(maxWidth: .infinity, minHeight: Tokens.Size.tapTarget)
            }
            .buttonStyle(.borderedProminent)
            .accessibilityIdentifier("trust.accept")
            Button("Not now") { model.dismiss(prompt) }
                .foregroundStyle(theme.textMuted)
        }
        .padding(Tokens.Space.xl)
    }
}

/// §6: the recipient is always asked to accept.
struct FileOfferView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme
    let offer: TransferInfo

    private var sender: String {
        model.snapshot.peers.first { $0.key == offer.peerKey }?.displayName ?? "Someone"
    }

    var body: some View {
        VStack(spacing: Tokens.Space.xl) {
            Image(systemName: "arrow.down.doc")
                .font(.system(size: 40))
                .foregroundStyle(theme.accent)
            Text("\(sender) wants to send you a file")
                .font(.token(Tokens.Typography.title))
                .foregroundStyle(theme.text)
                .multilineTextAlignment(.center)
            Text("\(offer.name) · \(ByteCountFormatter.string(fromByteCount: Int64(offer.size), countStyle: .file))")
                .font(.token(Tokens.Typography.body))
                .foregroundStyle(theme.textMuted)
            HStack(spacing: Tokens.Space.md) {
                Button("Decline") { model.respond(to: offer, accept: false) }
                    .frame(maxWidth: .infinity, minHeight: Tokens.Size.tapTarget)
                    .accessibilityIdentifier("file.decline")
                Button {
                    model.respond(to: offer, accept: true)
                } label: {
                    Text("Accept").frame(maxWidth: .infinity, minHeight: Tokens.Size.tapTarget)
                }
                .buttonStyle(.borderedProminent)
                .accessibilityIdentifier("file.accept")
            }
        }
        .padding(Tokens.Space.xl)
    }
}
