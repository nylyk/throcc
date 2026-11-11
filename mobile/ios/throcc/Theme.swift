import SwiftUI

/// The colours of a palette, with the one mixed from them that cards on a panel use.
struct Theme: Equatable {
    let bg, panel, raised, text, muted, accent, warning, good, bad: Color
    let isDark: Bool

    init(_ palette: Palette) {
        bg = Color(rgb: palette.bg)
        panel = Color(rgb: palette.panel)
        text = Color(rgb: palette.text)
        muted = Color(rgb: palette.muted)
        accent = Color(rgb: palette.accent)
        warning = Color(rgb: palette.warning)
        good = Color(rgb: palette.good)
        bad = Color(rgb: palette.bad)
        raised = panel.mix(with: text, by: 0.06)
        let channels = [16, 8, 0].map { Double(palette.bg >> UInt32($0) & 0xFF) / 255 }
        isDark = 0.2126 * channels[0] + 0.7152 * channels[1] + 0.0722 * channels[2] < 0.5
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
        Label { Text(title) } icon: { Image(systemName: symbol).foregroundStyle(Color.primary) }
    }
}

/// A list on the theme's background with its rows on the panel colour, one step lighter
/// each in a side pane.
struct ThemedList<Content: View>: View {
    @Environment(\.theme) private var theme
    @Environment(\.isSidePane) private var isSidePane
    @ViewBuilder let content: Content

    var body: some View {
        List { Group { content }.listRowBackground(isSidePane ? theme.raised : theme.panel) }
            .scrollContentBackground(.hidden)
            .background(isSidePane ? theme.panel : theme.bg)
    }
}

/// A form coloured as [`ThemedList`] colours a list.
struct ThemedForm<Content: View>: View {
    @Environment(\.theme) private var theme
    @ViewBuilder let content: Content

    var body: some View {
        Form { Group { content }.listRowBackground(theme.panel) }
            .scrollContentBackground(.hidden)
            .background(theme.bg)
    }
}
