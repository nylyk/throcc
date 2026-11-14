import SwiftUI

/// The colours of a palette and the ones the core mixes from them.
struct Theme: Equatable {
    let bg, panel, raised, line, text, muted, accent, warning, good, bad: Color
    let isDark: Bool

    init(_ palette: Palette) {
        let colors = themeColors(palette: palette)
        bg = Color(rgb: colors.bg)
        panel = Color(rgb: colors.panel)
        raised = Color(rgb: colors.raised)
        line = Color(rgb: colors.line)
        text = Color(rgb: colors.text)
        muted = Color(rgb: colors.muted)
        accent = Color(rgb: colors.accent)
        warning = Color(rgb: colors.warning)
        good = Color(rgb: colors.good)
        bad = Color(rgb: colors.bad)
        isDark = !isLightTheme(palette: palette)
    }
}

extension Color {
    init(rgb: UInt32) {
        let part = { (shift: UInt32) in Double(rgb >> shift & 0xFF) / 255 }
        self.init(red: part(16), green: part(8), blue: part(0))
    }
}

extension Color.Resolved {
    var rgb: UInt32 {
        let byte = { (part: Float) in UInt32((min(max(part, 0), 1) * 255).rounded()) }
        return byte(red) << 16 | byte(green) << 8 | byte(blue)
    }
}

extension EnvironmentValues {
    @Entry var theme = Theme(Palette(bg: 0, panel: 0, text: 0xFFFFFF, muted: 0x808080, accent: 0, warning: 0, good: 0, bad: 0))
    /// Whether the view sits in the pane beside the main one, which is on the panel colour.
    @Entry var isSidePane = false
}

/// One of the theme's colours, as a style.
struct ThemeColor: ShapeStyle {
    let pick: @Sendable (Theme) -> Color

    func resolve(in environment: EnvironmentValues) -> Color {
        pick(environment.theme)
    }
}

extension ShapeStyle where Self == ThemeColor {
    static var bg: ThemeColor { ThemeColor { $0.bg } }
    static var panel: ThemeColor { ThemeColor { $0.panel } }
    static var raised: ThemeColor { ThemeColor { $0.raised } }
    static var line: ThemeColor { ThemeColor { $0.line } }
    static var text: ThemeColor { ThemeColor { $0.text } }
    static var muted: ThemeColor { ThemeColor { $0.muted } }
    static var warning: ThemeColor { ThemeColor { $0.warning } }
    static var good: ThemeColor { ThemeColor { $0.good } }
    static var bad: ThemeColor { ThemeColor { $0.bad } }
}

/// The theme for everything inside: its colours, light or dark, and the tint.
struct Themed: ViewModifier {
    @Environment(AppModel.self) private var model

    func body(content: Content) -> some View {
        let theme = model.theme
        content
            .environment(\.theme, theme)
            .tint(theme.accent)
            .preferredColorScheme(theme.isDark ? .dark : .light)
    }
}

/// A label whose icon is drawn in the text colour, whatever tints its title.
struct IconLabel: View {
    let title: LocalizedStringKey
    let symbol: String

    init(_ title: LocalizedStringKey, symbol: String) {
        self.title = title
        self.symbol = symbol
    }

    var body: some View {
        Label { Text(title) } icon: { Image(systemName: symbol).foregroundStyle(.text) }
    }
}

/// A button for a destructive action, titled in the theme's bad colour.
struct DestructiveButton: View {
    let title: LocalizedStringKey
    let action: () -> Void

    init(_ title: LocalizedStringKey, action: @escaping () -> Void) {
        self.title = title
        self.action = action
    }

    var body: some View {
        Button(role: .destructive, action: action) { Text(title).foregroundStyle(.bad) }
    }
}

/// A text field in the text colour, with its title as the prompt in the muted colour.
struct ThemedField: View {
    @Environment(\.theme) private var theme
    let title: LocalizedStringKey
    @Binding var text: String
    let isSecret: Bool

    init(_ title: LocalizedStringKey, text: Binding<String>, isSecret: Bool = false) {
        self.title = title
        _text = text
        self.isSecret = isSecret
    }

    var body: some View {
        let prompt = Text(title).foregroundStyle(theme.muted)
        Group {
            if isSecret {
                SecureField(title, text: $text, prompt: prompt)
            } else {
                TextField(title, text: $text, prompt: prompt)
            }
        }
        .foregroundStyle(.text)
    }
}

/// A sheet's confirming button, filled with the accent and titled in the background colour.
struct ConfirmButton: View {
    @Environment(\.theme) private var theme
    @Environment(\.isEnabled) private var isEnabled
    let title: LocalizedStringKey
    let action: () -> Void

    init(_ title: LocalizedStringKey, action: @escaping () -> Void) {
        self.title = title
        self.action = action
    }

    var body: some View {
        Button(action: action) { Text(title).foregroundStyle(isEnabled ? theme.bg : theme.muted) }
            .buttonStyle(.glassProminent)
    }
}

extension View {
    /// Gives a text button's label the 44 pt a finger needs.
    nonisolated func touchTarget() -> some View {
        frame(minWidth: 44, minHeight: 44).contentShape(.rect)
    }
}

/// A list on the theme's background with its rows on the panel colour, one step lighter
/// each in a side pane.
struct ThemedList<Content: View>: View {
    @Environment(\.theme) private var theme
    @Environment(\.isSidePane) private var isSidePane
    @ViewBuilder let content: Content

    var body: some View {
        List {
            Group { content }
                .listRowBackground(isSidePane ? theme.raised : theme.panel)
                .listRowSeparatorTint(theme.line)
        }
        // A reused row can keep the default separator after a theme change; a new list colours every row anew.
        .id(theme.line)
        .scrollContentBackground(.hidden)
        .background(isSidePane ? theme.panel : theme.bg)
    }
}

/// A form coloured as `ThemedList` colours a list.
struct ThemedForm<Content: View>: View {
    @Environment(\.theme) private var theme
    @ViewBuilder let content: Content

    var body: some View {
        Form {
            Group { content }
                .listRowBackground(theme.panel)
                .listRowSeparatorTint(theme.line)
        }
        // As in `ThemedList`.
        .id(theme.line)
        .scrollContentBackground(.hidden)
        .background(theme.bg)
    }
}
