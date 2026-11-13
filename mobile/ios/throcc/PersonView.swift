import SwiftUI

struct PersonView: View {
    @Environment(AppModel.self) private var model
    let user: UInt64
    /// The safety-number comparison on screen, which keeps the number on the key being compared.
    @State private var check: SafetyCheck?
    @State private var groups: [String] = []
    @State private var keyChangedWhileOpen = false
    @State private var removal: Removal?

    var body: some View {
        Group {
            if let person = model.state.person(user) {
                form(for: person)
            } else {
                ContentUnavailableView("No longer on this server", systemImage: "person.slash")
                    .foregroundStyle(.text)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .background(.bg)
            }
        }
        .navigationBarTitleDisplayMode(.inline)
        .onChange(of: model.state, initial: true) { follow() }
    }

    private func form(for person: Person) -> some View {
        let mine = model.state.me == user
        return ThemedForm {
            Section {
                VStack(spacing: 8) {
                    PersonPicture(person: person, size: 96)
                    Text(person.name)
                        .font(.title2.weight(.semibold))
                        .foregroundStyle(person.blocked ? ThemeColor.muted : .text)
                        .multilineTextAlignment(.center)
                    Text(person.role.title).foregroundStyle(.muted)
                }
                .frame(maxWidth: .infinity)
                .listRowBackground(Color.clear)
            }
            if person.blocked {
                Button("Unblock") { model.core.command(command: .unblock(user: user)) }
            } else if mine {
                Text("To verify your safety number with someone, open their profile while they open yours. You have a separate number with each person.")
                    .foregroundStyle(.muted)
            } else {
                keyWarning(for: person)
                safetyNumber(of: person)
                Section {
                    SoundControl(user: user, sound: .voice, level: person.voice)
                } header: {
                    Text("Voice").foregroundStyle(.muted)
                }
                if person.sharing?.audio == true {
                    Section {
                        SoundControl(user: user, sound: .share, level: person.shareAudio)
                    } header: {
                        Text("Stream Audio").foregroundStyle(.muted)
                    }
                }
                DestructiveButton("Block") { model.core.command(command: .block(user: user)) }
            }
            if !mine {
                moderation(of: person)
            }
        }
        .modifier(RemovalConfirmation(removal: $removal))
    }

    @ViewBuilder
    private func keyWarning(for person: Person) -> some View {
        if let warning = model.state.keyWarnings.first(where: { $0.user == user }) {
            Section {
                Text("\(person.name)’s new key doesn’t match the one you verified. Someone might be listening in.")
                    .foregroundStyle(.text)
                LabeledContent("Before") { Text(fingerprintShort(key: warning.verified)).monospaced().foregroundStyle(.muted) }
                    .foregroundStyle(.text)
                LabeledContent("Now") { Text(fingerprintShort(key: warning.now)).monospaced().foregroundStyle(.muted) }
                    .foregroundStyle(.text)
                Button("Dismiss Warning") { check?.dismissWarning() }
                    .disabled(check?.canDismissWarning() != true)
            } header: {
                Label("\(person.name)’s key changed", systemImage: "exclamationmark.triangle").foregroundStyle(.bad)
            }
        }
    }

    private func safetyNumber(of person: Person) -> some View {
        Section {
            HStack(spacing: 8) {
                TrustLabel(trust: person.trust)
                if let verifiedAs = person.verifiedAs {
                    Text("Verified as \(verifiedAs)")
                        .font(.caption)
                        .foregroundStyle(.muted)
                }
            }
            if keyChangedWhileOpen {
                Text("\(person.name)’s key changed while this was open, so this is a new number. Compare it again.")
                    .foregroundStyle(.text)
                Button("Compare Again") {
                    check?.compareAgain()
                    showCheck()
                }
            }
            if groups.isEmpty {
                Text("Their safety number appears once you share a room.").foregroundStyle(.muted)
            } else {
                SafetyNumber(groups: groups)
            }
            Button(person.verified ? "Mark as Not Verified" : "Mark as Verified") {
                check?.mark(verify: !person.verified)
            }
            .disabled(check?.canMark() != true)
        } header: {
            Text("Safety Number").foregroundStyle(.muted)
        } footer: {
            if let explanation = check?.explanation(name: person.name) {
                Text(explanation).foregroundStyle(.muted)
            }
        }
    }

    @ViewBuilder
    private func moderation(of person: Person) -> some View {
        if let role = model.state.role {
            Section {
                if let room = person.room, canKickFromRoom(actor: role, target: person.role) {
                    DestructiveButton("Kick from Room") {
                        model.core.command(command: .kickFromRoom(user: user, room: room))
                    }
                }
                if canRemove(actor: role, target: person.role) {
                    DestructiveButton("Remove from Server") { removal = Removal(user: user, name: person.name) }
                }
            }
        }
    }

    /// Keeps the number on the key the person presents now.
    private func follow() {
        let check = self.check ?? model.core.safetyCheck(user: user)
        if check.follow() || self.check == nil {
            self.check = check
            showCheck()
        }
    }

    private func showCheck() {
        guard let check else { return }
        groups = check.groups()
        keyChangedWhileOpen = check.keyChangedWhileOpen()
    }
}

/// The safety number's twelve groups of five digits.
private struct SafetyNumber: View {
    @Environment(\.dynamicTypeSize) private var typeSize
    let groups: [String]

    var body: some View {
        let columns = typeSize.isAccessibilitySize ? 2 : 4
        LazyVGrid(columns: Array(repeating: GridItem(.flexible()), count: columns), spacing: 8) {
            ForEach(groups.indices, id: \.self) { Text(groups[$0]).speechSpellsOutCharacters() }
        }
        .font(.title3)
        .monospaced()
        .foregroundStyle(.text)
        .lineLimit(1)
        .minimumScaleFactor(0.5)
        .padding(.vertical, 8)
    }
}

/// The volume of one of someone's sounds here, from 0 to 200% on the loudness curve every
/// client uses, with its mute switch.
private struct SoundControl: View {
    @Environment(AppModel.self) private var model
    let user: UInt64
    let sound: Sound
    let level: SoundLevel
    @State private var loudness: Float

    init(user: UInt64, sound: Sound, level: SoundLevel) {
        self.user = user
        self.sound = sound
        self.level = level
        _loudness = State(initialValue: gainToLoudness(gain: level.volume))
    }

    var body: some View {
        let percent = "\(Int((loudness * 100).rounded()))%"
        LabeledContent("Volume") { Text(percent).foregroundStyle(.muted) }
            .foregroundStyle(.text)
        Slider(value: $loudness, in: 0...2) { Text("Volume") }
            .accessibilityValue(percent)
            .onChange(of: loudness) { set(SoundLevel(volume: loudnessToGain(loudness: loudness), muted: level.muted)) }
        Toggle("Mute", isOn: Binding { level.muted } set: {
            set(SoundLevel(volume: loudnessToGain(loudness: loudness), muted: $0))
        })
        .foregroundStyle(.text)
    }

    private func set(_ level: SoundLevel) {
        model.core.command(command: .setSoundLevel(user: user, sound: sound, level: level))
    }
}

struct Removal {
    let user: UInt64
    let name: String
}

struct RemovalConfirmation: ViewModifier {
    @Environment(AppModel.self) private var model
    @Binding var removal: Removal?

    func body(content: Content) -> some View {
        content.confirmationDialog(
            "Remove “\(removal?.name ?? "")”?",
            isPresented: $removal.isPresent(),
            titleVisibility: .visible,
            presenting: removal
        ) { removal in
            Button("Remove", role: .destructive) {
                model.core.command(command: .removeUser(user: removal.user, cascade: false))
            }
            Button("Remove with Everyone They Invited", role: .destructive) {
                model.core.command(command: .removeUser(user: removal.user, cascade: true))
            }
        } message: { _ in
            Text("They are disconnected at once and need a new invite to come back.")
        }
    }
}
