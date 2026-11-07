import SwiftUI

extension Color {
    init(_ c: RGBA) {
        self.init(.sRGB, red: c.r, green: c.g, blue: c.b, opacity: c.a)
    }
}

/// User-selectable appearance (§9: dark default, light available).
enum Appearance: String, CaseIterable, Identifiable {
    case dark
    case light
    case system

    var id: String {
        rawValue
    }

    var colorScheme: ColorScheme? {
        switch self {
        case .dark: .dark
        case .light: .light
        case .system: nil
        }
    }

    var label: String {
        switch self {
        case .dark: "Dark"
        case .light: "Light"
        case .system: "System"
        }
    }
}

/// Resolved colours for the current colour scheme.
struct Theme {
    let palette: Palette

    init(_ scheme: ColorScheme) {
        palette = scheme == .light ? Tokens.light : Tokens.dark
    }

    var accent: Color {
        Color(palette.accent)
    }

    var onAccent: Color {
        Color(palette.onAccent)
    }

    var bg: Color {
        Color(palette.bg)
    }

    var surface: Color {
        Color(palette.surface)
    }

    var surfaceRaised: Color {
        Color(palette.surfaceRaised)
    }

    var border: Color {
        Color(palette.border)
    }

    var text: Color {
        Color(palette.text)
    }

    var textMuted: Color {
        Color(palette.textMuted)
    }

    var danger: Color {
        Color(palette.danger)
    }

    var success: Color {
        Color(palette.success)
    }

    var speakingGlow: Color {
        Color(palette.speakingGlow)
    }
}

private struct ThemeKey: EnvironmentKey {
    static let defaultValue = Theme(.dark)
}

extension EnvironmentValues {
    var theme: Theme {
        get { self[ThemeKey.self] }
        set { self[ThemeKey.self] = newValue }
    }
}

/// Injects the token palette matching the effective colour scheme.
struct ThemeProvider: ViewModifier {
    @Environment(\.colorScheme) private var scheme

    func body(content: Content) -> some View {
        content
            .environment(\.theme, Theme(scheme))
            .tint(Theme(scheme).accent)
    }
}

extension Font {
    /// Inter (bundled from design/fonts) at a token size; scales with Dynamic Type.
    static func token(_ style: Tokens.TypeStyle) -> Font {
        let name = style.weight >= 600 ? "Inter-SemiBold" : "Inter-Regular"
        return .custom(name, size: style.size, relativeTo: .body)
    }
}

extension View {
    func themed() -> some View {
        modifier(ThemeProvider())
    }

    /// Card surface used across screens.
    func card(_ theme: Theme) -> some View {
        padding(Tokens.Space.lg)
            .background(theme.surface, in: RoundedRectangle(cornerRadius: Tokens.Radius.lg, style: .continuous))
    }
}
