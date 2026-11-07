import SwiftUI

/// One prompt at a time: trust a new/changed device, or accept an incoming file.
enum PromptItem: Identifiable {
    case trust(TrustPromptItem)
    case file(TransferInfo)

    var id: String {
        switch self {
        case let .trust(t): "trust-\(t.id)"
        case let .file(f): "file-\(f.id)"
        }
    }
}

extension AppModel {
    var nextPrompt: PromptItem? {
        if let t = trustPrompts.first {
            return .trust(t)
        }
        if let f = fileOffers.first {
            return .file(f)
        }
        return nil
    }
}

/// Presents trust and file-offer sheets from whichever screen is on top.
struct PromptsModifier: ViewModifier {
    @EnvironmentObject private var model: AppModel
    let active: Bool

    func body(content: Content) -> some View {
        content.sheet(item: Binding(
            get: { active ? model.nextPrompt : nil },
            set: { _ in }
        )) { item in
            Group {
                switch item {
                case let .trust(t): TrustPromptView(prompt: t)
                case let .file(f): FileOfferView(offer: f)
                }
            }
            .themed()
            .presentationDetents([.medium])
            .interactiveDismissDisabled()
        }
    }
}

extension View {
    func prompts(active: Bool) -> some View {
        modifier(PromptsModifier(active: active))
    }
}

struct RootView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.theme) private var theme

    var body: some View {
        NavigationStack {
            HomeView()
        }
        .prompts(active: !model.inCall)
        .fullScreenCover(isPresented: Binding(get: { model.inCall }, set: { _ in })) {
            NavigationStack {
                CallView()
            }
            .themed()
            .prompts(active: true)
        }
        .alert("CallXL", isPresented: Binding(
            get: { model.lastError != nil && !model.inCall },
            set: {
                if !$0 {
                    model.lastError = nil
                }
            }
        )) {
            Button("OK", role: .cancel) { model.lastError = nil }
        } message: {
            Text(model.lastError ?? "")
        }
        .task {
            model.start()
        }
    }
}
