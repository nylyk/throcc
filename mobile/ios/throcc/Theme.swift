import SwiftUI

/// The colours of a palette and the ones mixed from them. The text and status colours
/// read at 4.5:1 on every surface.
struct Theme: Equatable {
    let bg, panel, raised, line, text, muted, accent, warning, good, bad: Color
    let isDark: Bool

    init(_ palette: Palette) {
        let text = Color(rgb: palette.text)
        let bg = Color(rgb: palette.bg)
        let panel = Color(rgb: palette.panel)
        let raised = panel.mix(with: text, by: 0.06)
        let readable = { (rgb: UInt32) -> Color in
            let color = Color(rgb: rgb)
            let mixes = stride(from: 0, through: 1, by: 0.1).lazy.map { color.mix(with: text, by: $0) }
            return mixes.first { mixed in [bg, panel, raised].allSatisfy { contrast(mixed, $0) >= 4.5 } } ?? text
        }
        self.text = text
        self.bg = bg
        self.panel = panel
        self.raised = raised
        line = panel.mix(with: text, by: 0.16)
        muted = readable(palette.muted)
        accent = readable(palette.accent)
        warning = readable(palette.warning)
        good = readable(palette.good)
        bad = readable(palette.bad)
        isDark = !isLightTheme(palette: palette)
    }
}

/// The WCAG contrast ratio of two colours.
private func contrast(_ one: Color, _ other: Color) -> Float {
    let luminances = [one, other].map { color in
        let parts = color.resolve(in: EnvironmentValues())
        return 0.2126 * parts.linearRed + 0.7152 * parts.linearGreen + 0.0722 * parts.linearBlue
    }
    return (max(luminances[0], luminances[1]) + 0.05) / (min(luminances[0], luminances[1]) + 0.05)
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
        .scrollContentBackground(.hidden)
        .background(isSidePane ? theme.panel : theme.bg)
    }
}

/// A form coloured as [`ThemedList`] colours a list.
struct ThemedForm<Content: View>: View {
    @Environment(\.theme) private var theme
    @ViewBuilder let content: Content

    var body: some View {
        Form {
            Group { content }
                .listRowBackground(theme.panel)
                .listRowSeparatorTint(theme.line)
        }
        .scrollContentBackground(.hidden)
        .background(theme.bg)
    }
}
