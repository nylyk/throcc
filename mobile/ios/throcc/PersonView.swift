import SwiftUI

struct PersonView: View {
    @Environment(AppModel.self) private var model
    let user: UInt64
    /// The identity key whose safety number is shown, the only one a tap may verify.
    @State private var shownKey: Data?
    @State private var number: String?
    @State private var keyChangedWhileOpen = false
    @State private var removal: Removal?
    @State private var confirmsRemoval = false

    var body: some View {
        Group {
            if let person = model.state.person(user) {
                form(for: person)
            } else {
                ContentUnavailableView("No longer on this server", systemImage: "person.slash")
            }
        }
        .navigationBarTitleDisplayMode(.inline)
        .toolbar { CloseButton() }
    }

    private func form(for person: Person) -> some View {
        let mine = model.state.me == user
        return ThemedForm {
            Section {
                VStack(spacing: 8) {
                    PersonPicture(person: person, size: 96)
                    Text(person.name).font(.title2.weight(.semibold))
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
                trust(in: person)
                Section("Voice") {
                    SoundControl(user: user, sound: .voice, level: person.voice)
                }
                if person.sharing?.audio == true {
                    Section("Stream audio") {
                        SoundControl(user: user, sound: .share, level: person.shareAudio)
                    }
                }
                Button("Block", role: .destructive) { model.core.command(command: .block(user: user)) }
            }
            if !mine {
                moderation(of: person)
            }
        }
        .onChange(of: person.identityKey, initial: true) { follow(person.identityKey) }
        .modifier(RemovalConfirmation(removal: removal, isPresented: $confirmsRemoval))
    }

    @ViewBuilder
    private func keyWarning(for person: Person) -> some View {
        if let warning = model.state.keyWarnings.first(where: { $0.user == user }) {
            Section {
                Text("\(person.name)’s new key doesn’t match the one you verified. Someone might be listening in.")
                LabeledContent("Before") { Text(fingerprintShort(key: warning.verified)).monospaced() }
                LabeledContent("Now") { Text(fingerprintShort(key: warning.now)).monospaced() }
                Button("Dismiss Warning") {
                    model.core.command(command: .dismissKeyChange(user: user, shown: shownKey))
                }
                .disabled(keyChangedWhileOpen || shownKey != person.identityKey)
            } header: {
                Text("\(person.name)’s key changed").foregroundStyle(.bad)
            }
        }
    }

    private func trust(in person: Person) -> some View {
        Section {
            TrustLabel(trust: model.state.trust(of: person))
            if let number, let shownKey {
                SafetyNumber(number: number)
                if keyChangedWhileOpen {
                    Text("\(person.name)’s key changed while this was open, so this is a new number. Compare it again.")
                    Button("Compare Again") { keyChangedWhileOpen = false }
                }
                Button(person.verified ? "Mark as Not Verified" : "Mark as Verified") {
                    let verified = !person.verified
                    model.core.command(command: .setVerified(user: user, key: shownKey, verified: verified))
                }
                .disabled(keyChangedWhileOpen || shownKey != person.identityKey)
            } else {
                Text("Their safety number appears once you share a room.").foregroundStyle(.muted)
            }
        } header: {
            Text("Safety Number")
        } footer: {
            if number != nil {
                Text("To check that your calls with \(person.name) are end-to-end encrypted, compare these numbers with theirs in person or through another app.")
            }
        }
    }

    @ViewBuilder
    private func moderation(of person: Person) -> some View {
        if let role = model.state.role {
            Section {
                if let room = person.room, canKickFromRoom(actor: role, target: person.role) {
                    Button("Kick from Room", role: .destructive) {
                        model.core.command(command: .kickFromRoom(user: user, room: room))
                    }
                }
                if canRemove(actor: role, target: person.role) {
                    Button("Remove from Server", role: .destructive) {
                        removal = Removal(user: user, name: person.name)
                        confirmsRemoval = true
                    }
                }
            }
        }
    }

    /// Keeps the number on the key the person presents, and notes a change under the user's eyes.
    private func follow(_ key: Data?) {
        guard let key, key != shownKey else { return }
        keyChangedWhileOpen = keyChangedWhileOpen || shownKey != nil
        shownKey = key
        number = model.core.safetyNumber(key: key)
    }
}

private struct SafetyNumber: View {
    let number: String

    var body: some View {
        let groups = stride(from: 0, to: number.count, by: 5).map { number.dropFirst($0).prefix(5) }
        LazyVGrid(columns: Array(repeating: GridItem(.flexible()), count: 4), spacing: 8) {
            ForEach(groups.indices, id: \.self) { Text(groups[$0]) }
        }
        .font(.title3)
        .monospaced()
        .padding(.vertical, 8)
    }
}

/// The volume of one of someone's sounds here, from 0 to 200%, with its mute switch.
private struct SoundControl: View {
    @Environment(AppModel.self) private var model
    let user: UInt64
    let sound: Sound
    let level: SoundLevel
    @State private var volume: Float

    init(user: UInt64, sound: Sound, level: SoundLevel) {
        self.user = user
        self.sound = sound
        self.level = level
        _volume = SwiftUI.State(initialValue: min(level.volume, 2))
    }

    var body: some View {
        LabeledContent("Volume", value: "\(Int((volume * 100).rounded()))%")
        Slider(value: $volume, in: 0...2)
            .onChange(of: volume) { set(SoundLevel(volume: volume, muted: level.muted)) }
        Toggle("Mute", isOn: Binding { level.muted } set: { set(SoundLevel(volume: volume, muted: $0)) })
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
    let removal: Removal?
    @Binding var isPresented: Bool

    func body(content: Content) -> some View {
        content.confirmationDialog(
            "Remove “\(removal?.name ?? "")”?",
            isPresented: $isPresented,
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
