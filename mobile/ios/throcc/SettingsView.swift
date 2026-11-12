import SwiftUI

struct SettingsView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        @Bindable var model = model
        NavigationStack(path: $model.settingsPath) {
            ThemedList {
                Group {
                    NavigationLink(value: SettingsPage.profile) {
                        IconLabel("Profile", symbol: "person.crop.circle")
                    }
                    NavigationLink(value: SettingsPage.appearance) {
                        IconLabel("Appearance", symbol: "paintpalette")
                    }
                    NavigationLink(value: SettingsPage.verified) {
                        IconLabel("Verified People", symbol: "checkmark.shield")
                    }
                    NavigationLink(value: SettingsPage.blocked) {
                        IconLabel("Blocked People", symbol: "nosign")
                    }
                    NavigationLink(value: SettingsPage.about) {
                        IconLabel("About", symbol: "info.circle")
                    }
                }
                .foregroundStyle(.text)
            }
            .navigationTitle("Settings")
            .toolbar { CloseButton() }
            .navigationDestination(for: SettingsPage.self) { page in
                switch page {
                case .profile: ProfilePage()
                case .appearance: AppearancePage()
                case .verified: VerifiedPage()
                case .blocked: BlockedPage()
                case .about: AboutPage()
                }
            }
        }
    }
}

private struct ProfilePage: View {
    @Environment(AppModel.self) private var model
    @State private var name = ""
    @State private var picture: Data?
    @State private var status: SaveStatus?

    var body: some View {
        let check = checkName(typed: name)
        let changed = check.name != model.profile?.name || picture != model.profile?.picture
        ThemedForm {
            Section {
                PictureEditor(picture: $picture, name: name)
                ThemedField("Name", text: $name)
                    .textContentType(.nickname)
                    .autocorrectionDisabled()
            } footer: {
                if let problem = check.problem {
                    Text(problem).foregroundStyle(.bad)
                }
            }
            Section {
                Button("Save") {
                    do {
                        try model.setProfile(name: name, picture: picture)
                        status = .saved
                    } catch {
                        status = .refused(error.explanation)
                    }
                }
                .disabled(check.name == nil || !changed)
            } footer: {
                SaveStatusText(status: changed ? nil : status)
            }
        }
        .navigationTitle("Profile")
        .onAppear {
            name = model.profile?.name ?? ""
            picture = model.profile?.picture
        }
    }
}

struct SaveStatusText: View {
    let status: SaveStatus?

    var body: some View {
        switch status {
        case .saving: Text("Saving…").foregroundStyle(.muted)
        case .saved: Text("Saved").foregroundStyle(.muted)
        case .refused(let reason): Text(reason).foregroundStyle(.bad)
        case nil: EmptyView()
        }
    }
}

private struct AppearancePage: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let custom = ThemeChoice(name: customThemeName(), palette: model.customPalette)
        ThemedList {
            Section {
                ForEach(model.themes + [custom], id: \.name) { choice in
                    let picked = choice.name == model.pickedTheme.name
                    Button { pick(choice) } label: {
                        HStack(spacing: 12) {
                            Swatch(palette: choice.palette)
                            Text(choice.name).foregroundStyle(.text)
                            Spacer()
                            if picked {
                                Image(systemName: "checkmark")
                                    .foregroundStyle(.tint)
                                    .accessibilityHidden(true)
                            }
                        }
                    }
                    .accessibilityAddTraits(picked ? .isSelected : [])
                }
            } header: {
                Text("Theme").foregroundStyle(.muted)
            }
            if model.pickedTheme.name == customThemeName() {
                Section {
                    well("Background", \.bg)
                    well("Panel", \.panel)
                    well("Text", \.text)
                    well("Muted text", \.muted)
                    well("Accent", \.accent)
                    well("Warning", \.warning)
                    well("Good", \.good)
                    well("Bad", \.bad)
                } header: {
                    Text("Colors").foregroundStyle(.muted)
                }
            }
        }
        .navigationTitle("Appearance")
        .onDisappear { model.saveCustomTheme() }
    }

    private func pick(_ choice: ThemeChoice) {
        if choice.name == customThemeName() {
            model.useCustomTheme(choice.palette)
        } else {
            model.useTheme(named: choice.name)
        }
    }

    private func well(_ title: LocalizedStringKey, _ color: WritableKeyPath<Palette, UInt32> & Sendable) -> some View {
        let selection = Binding {
            Color(rgb: model.customPalette[keyPath: color])
        } set: { picked in
            var palette = model.customPalette
            palette[keyPath: color] = picked.resolve(in: EnvironmentValues()).rgb
            if palette != model.customPalette {
                model.useCustomTheme(palette)
            }
        }
        return ColorPicker(selection: selection, supportsOpacity: false) { Text(title).foregroundStyle(.text) }
    }
}

/// The eight colours of a palette: its text and status colours on its panel, on its background.
private struct Swatch: View {
    let palette: Palette

    var body: some View {
        let dots = [palette.text, palette.muted, palette.accent, palette.warning, palette.good, palette.bad]
        HStack(spacing: 4) {
            ForEach(dots.indices, id: \.self) { dot in
                Circle()
                    .fill(Color(rgb: dots[dot]))
                    .frame(width: 8, height: 8)
            }
        }
        .padding(8)
        .background(Color(rgb: palette.panel), in: .capsule)
        .padding(4)
        .background(Color(rgb: palette.bg), in: .rect(cornerRadius: 16))
        .overlay { RoundedRectangle(cornerRadius: 16).strokeBorder(.line) }
        .accessibilityHidden(true)
    }
}

private struct VerifiedPage: View {
    @Environment(AppModel.self) private var model
    @State private var people: [VerifiedPerson] = []
    @State private var blocked: Set<Data> = []

    var body: some View {
        ThemedList {
            Section {
                ForEach(people, id: \.key) { person in
                    HStack {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(blocked.contains(person.key) ? blockedName() : person.name).foregroundStyle(.text)
                            Text(person.servers.joined(separator: ", "))
                                .font(.caption)
                                .foregroundStyle(.muted)
                        }
                        Spacer()
                        ShortKey(key: person.key)
                    }
                    .swipeActions {
                        Button("Remove", systemImage: "minus.circle") {
                            model.attempt { try model.core.unverify(key: person.key) }
                            load()
                        }
                        .tint(.red)
                    }
                }
            } header: {
                Text("People that you have verified").foregroundStyle(.muted)
            } footer: {
                if people.isEmpty {
                    Text("Nobody yet. Compare safety numbers with someone to verify them.").foregroundStyle(.muted)
                }
            }
        }
        .navigationTitle("Verified People")
        .onAppear(perform: load)
    }

    private func load() {
        people = model.core.verified()
        blocked = Set(model.core.blocked().map(\.key))
    }
}

private struct BlockedPage: View {
    @Environment(AppModel.self) private var model
    @State private var people: [BlockedPerson] = []

    var body: some View {
        ThemedList {
            Section {
                ForEach(people, id: \.key) { person in
                    HStack(spacing: 12) {
                        Avatar(picture: model.picture(in: person.picture), name: person.name, size: 28)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(person.name).foregroundStyle(.text)
                            if let server = person.server {
                                Text(server)
                                    .font(.caption)
                                    .foregroundStyle(.muted)
                            }
                        }
                        Spacer()
                        ShortKey(key: person.key)
                        Button {
                            model.attempt { try model.core.unblock(key: person.key) }
                            people = model.core.blocked()
                        } label: {
                            Text("Unblock").touchTarget()
                        }
                        .buttonStyle(.borderless)
                    }
                }
            } header: {
                Text("People that you have blocked").foregroundStyle(.muted)
            } footer: {
                if people.isEmpty {
                    Text("Nobody. Tap someone in a room to block them.").foregroundStyle(.muted)
                }
            }
        }
        .navigationTitle("Blocked People")
        .onAppear { people = model.core.blocked() }
    }
}

struct ShortKey: View {
    let key: Data

    var body: some View {
        Text(fingerprintShort(key: key))
            .font(.caption)
            .monospaced()
            .foregroundStyle(.muted)
    }
}

private struct AboutPage: View {
    var body: some View {
        ThemedList {
            LabeledContent("Version") {
                Text(Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "").foregroundStyle(.muted)
            }
            .foregroundStyle(.text)
            if let source = URL(string: "https://github.com/nylyk/throcc") {
                Link("Source", destination: source)
            }
        }
        .navigationTitle("About")
    }
}
