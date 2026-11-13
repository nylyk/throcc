import SwiftUI

struct SettingsView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        @Bindable var model = model
        NavigationStack(path: $model.settingsPath) {
            ThemedList {
                Section {
                    NavigationLink(value: SettingsPage.profile) {
                        IconLabel("Profile", symbol: "person.crop.circle")
                    }
                    NavigationLink(value: SettingsPage.voice) {
                        IconLabel("Voice", symbol: "mic")
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
                } footer: {
                    if let problem = model.storeProblems.settings {
                        StoreProblem(text: problem)
                    }
                }
                .foregroundStyle(.text)
            }
            .navigationTitle("Settings")
            .toolbar { CloseButton() }
            .navigationDestination(for: SettingsPage.self) { page in
                switch page {
                case .profile: ProfilePage()
                case .voice: VoicePage()
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
                if let problem = nameProblem(typed: name) {
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

private struct VoicePage: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let voice = model.voice
        ThemedForm {
            Section {
                VolumeSlider("Input volume", value: setting(\.inputVolume)) { model.setVoice(model.voice, keep: true) }
                VolumeSlider("Output volume", value: setting(\.outputVolume)) { model.setVoice(model.voice, keep: true) }
            } header: {
                Text("Volume").foregroundStyle(.muted)
            }
            Section {
                LabeledContent("Sensitivity") { Text(decibels(voice.sensitivityDbfs)).foregroundStyle(.muted) }
                    .foregroundStyle(.text)
                VStack(spacing: 12) {
                    TimelineView(.periodic(from: .now, by: 0.1)) { _ in
                        LevelMeter(level: model.core.micLevel(), sensitivity: voice.sensitivityDbfs)
                    }
                    Slider(value: setting(\.sensitivityDbfs), in: quietestGateDbfs()...0) {
                        Text("Sensitivity")
                    } onEditingChanged: { editing in
                        if !editing { model.setVoice(model.voice, keep: true) }
                    }
                    .accessibilityValue(decibels(voice.sensitivityDbfs))
                }
                Toggle("Speech detection", isOn: setting(\.speechOnly, keep: true))
                    .foregroundStyle(.text)
            } header: {
                Text("Voice activation").foregroundStyle(.muted)
            }
            Section {
                Picker("Noise suppression", selection: setting(\.noiseSuppression, keep: true)) {
                    Text("Off").tag(NoiseSuppression.off)
                    Text("Low").tag(NoiseSuppression.low)
                    Text("Moderate").tag(NoiseSuppression.moderate)
                    Text("High").tag(NoiseSuppression.high)
                }
                .foregroundStyle(.text)
                Toggle("Automatic gain", isOn: setting(\.automaticGain, keep: true))
                    .foregroundStyle(.text)
            } header: {
                Text("Processing").foregroundStyle(.muted)
            }
        }
        .navigationTitle("Voice")
    }

    /// One of the voice settings, applied as it changes and kept when `keep`. The sensitivity
    /// moves in whole decibels.
    private func setting<Value>(_ path: WritableKeyPath<Voice, Value>, keep: Bool = false) -> Binding<Value> {
        Binding {
            model.voice[keyPath: path]
        } set: { value in
            var voice = model.voice
            voice[keyPath: path] = value
            voice.sensitivityDbfs.round()
            model.setVoice(voice, keep: keep)
        }
    }
}

private func decibels(_ dbfs: Float) -> String {
    "\(Int(dbfs.rounded())) dB".replacingOccurrences(of: "-", with: "−")
}

/// The mic level as a bar, in the good colour while the gate lets it through, with a mark at
/// `sensitivity`. Outside a call the bar stays empty.
private struct LevelMeter: View {
    let level: MicLevel?
    let sensitivity: Float

    var body: some View {
        GeometryReader { bar in
            ZStack(alignment: .leading) {
                Capsule().fill(.line)
                if let level {
                    Capsule()
                        .fill(level.gateOpen ? AnyShapeStyle(.good) : AnyShapeStyle(.muted))
                        .frame(width: bar.size.width * fraction(level.dbfs))
                }
                Rectangle()
                    .fill(.text)
                    .frame(width: 2)
                    .offset(x: bar.size.width * fraction(sensitivity) - 1)
            }
        }
        .frame(height: 8)
        .clipShape(.capsule)
        .accessibilityElement()
        .accessibilityLabel("Mic level")
        .accessibilityValue(level.map { $0.gateOpen ? "Sending" : "Quiet" } ?? "")
    }

    private func fraction(_ dbfs: Float) -> CGFloat {
        CGFloat(min(max(1 - dbfs / quietestGateDbfs(), 0), 1))
    }
}

/// A gain on the loudness curve every client uses, from 0 to 200 % with its percentage, which
/// calls `done` once let go.
private struct VolumeSlider: View {
    let title: LocalizedStringKey
    @Binding var value: Float
    let done: () -> Void

    init(_ title: LocalizedStringKey, value: Binding<Float>, done: @escaping () -> Void) {
        self.title = title
        _value = value
        self.done = done
    }

    var body: some View {
        let loudness = gainToLoudness(gain: value)
        let percent = "\(Int((loudness * 100).rounded()))%"
        LabeledContent(title) { Text(percent).foregroundStyle(.muted) }
            .foregroundStyle(.text)
        Slider(value: Binding { loudness } set: { value = loudnessToGain(loudness: $0) }, in: 0...2) {
            Text(title)
        } onEditingChanged: { editing in
            if !editing { done() }
        }
        .accessibilityValue(percent)
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
                VStack(alignment: .leading, spacing: 4) {
                    if let problem = model.storeProblems.verified {
                        StoreProblem(text: problem)
                    }
                    Text("People that you have verified").foregroundStyle(.muted)
                }
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
                        Avatar(picture: model.picture(in: person.picture), initial: initial(name: person.name), size: 28)
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
                VStack(alignment: .leading, spacing: 4) {
                    if let problem = model.storeProblems.blocked {
                        StoreProblem(text: problem)
                    }
                    Text("People that you have blocked").foregroundStyle(.muted)
                }
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

/// Why something this install keeps couldn't be read, so the app started without it.
private struct StoreProblem: View {
    let text: String

    var body: some View {
        Text(text)
            .font(.footnote)
            .textCase(nil)
            .foregroundStyle(.bad)
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
