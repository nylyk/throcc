import SwiftUI

struct SettingsView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        @Bindable var model = model
        NavigationStack(path: $model.settingsPath) {
            ThemedList {
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
                TextField("Name", text: $name)
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
        case .saving: Text("Saving…")
        case .saved: Text("Saved")
        case .refused(let reason): Text(reason).foregroundStyle(.bad)
        case nil: EmptyView()
        }
    }
}

private struct AppearancePage: View {
    @Environment(AppModel.self) private var model
    @Environment(\.self) private var environment

    var body: some View {
        let custom = Binding {
            model.tint
        } set: { color in
            model.useAccent(rgb: color.resolve(in: environment).rgb)
        }
        let accents = model.core.accents()
        ThemedList {
            Section("Accent") {
                ForEach(accents, id: \.name) { accent in
                    Button { model.useAccent(named: accent.name) } label: {
                        HStack(spacing: 12) {
                            Circle()
                                .fill(Color(rgb: accent.rgb))
                                .frame(width: 24, height: 24)
                            Text(accent.name).foregroundStyle(Color.primary)
                            Spacer()
                            if accent == model.accent {
                                Image(systemName: "checkmark")
                            }
                        }
                    }
                }
                ColorPicker(selection: custom, supportsOpacity: false) {
                    HStack {
                        Text("Custom")
                        Spacer()
                        if !accents.contains(model.accent) {
                            Image(systemName: "checkmark").foregroundStyle(.tint)
                        }
                    }
                }
            }
        }
        .navigationTitle("Appearance")
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
                            Text(blocked.contains(person.key) ? blockedName() : person.name)
                            Text(person.servers.joined(separator: ", "))
                                .font(.caption)
                                .foregroundStyle(.muted)
                        }
                        Spacer()
                        ShortKey(key: person.key)
                    }
                    .swipeActions {
                        Button("Remove") {
                            model.attempt { try model.core.unverify(key: person.key) }
                            load()
                        }
                        .tint(.red)
                    }
                }
            } header: {
                Text("People that you have verified")
            } footer: {
                if people.isEmpty {
                    Text("Nobody yet. Compare safety numbers with someone to verify them.")
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
                            Text(person.name)
                            if let server = person.server {
                                Text(server)
                                    .font(.caption)
                                    .foregroundStyle(.muted)
                            }
                        }
                        Spacer()
                        ShortKey(key: person.key)
                        Button("Unblock") {
                            model.attempt { try model.core.unblock(key: person.key) }
                            people = model.core.blocked()
                        }
                        .buttonStyle(.borderless)
                    }
                }
            } header: {
                Text("People that you have blocked")
            } footer: {
                if people.isEmpty {
                    Text("Nobody. Tap someone in a room to block them.")
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
            LabeledContent("Version", value: Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "")
            if let source = URL(string: "https://github.com/nylyk/throcc") {
                Link("Source", destination: source)
            }
        }
        .navigationTitle("About")
    }
}
