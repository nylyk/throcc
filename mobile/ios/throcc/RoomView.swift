import SwiftUI

struct RoomView: View {
    @Environment(AppModel.self) private var model

    private var fullscreen: Binding<Bool> {
        Binding(get: { model.fullscreenShare != nil }, set: { if !$0 { model.fullscreenShare = nil } })
    }

    var body: some View {
        let state = model.state
        if let current = state.current, let room = state.rooms.first(where: { $0.id == current.room }) {
            let people = state.people.filter { $0.room == room.id }
            let watched = people.filter { !$0.blocked && $0.sharing != nil && current.watching.contains($0.user) }
            GeometryReader { window in
                ScrollView {
                    ForEach(watched, id: \.user) { person in
                        if let share = person.sharing {
                            ShareTile(person: person, share: share, maxHeight: window.size.height * 0.6)
                                .padding([.horizontal, .top], 16)
                        }
                    }
                    LazyVGrid(columns: [GridItem(.adaptive(minimum: 150), spacing: 12)], spacing: 12) {
                        ForEach(people, id: \.user) { person in
                            PersonCard(person: person, watched: current.watching.contains(person.user))
                                .contentShape(.rect(cornerRadius: 16))
                                .onTapGesture { model.sheet = .person(person.user) }
                                .accessibilityElement(children: .contain)
                                .accessibilityAddTraits(.isButton)
                                .accessibilityAction { model.sheet = .person(person.user) }
                                .accessibilityValue(person.speaking ? "Speaking" : "")
                        }
                    }
                    .padding(16)
                }
            }
            .background(.bg)
            .fullScreenCover(isPresented: fullscreen) {
                if let user = model.fullscreenShare {
                    ShareFullscreen(user: user).environment(model)
                }
            }
            .navigationTitle(room.name)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItemGroup(placement: .bottomBar) {
                    MicButton()
                    SoundButton()
                    Spacer()
                    LeaveButton()
                }
            }
        } else {
            let (hint, symbol) = if state.server == nil {
                ("Pick a server on the left.", "server.rack")
            } else if state.rooms.isEmpty {
                ("There are no rooms yet.", "speaker.wave.2")
            } else {
                ("Join a room on the left.", "speaker.wave.2")
            }
            ContentUnavailableView(hint, systemImage: symbol)
                .foregroundStyle(.text)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .background(.bg)
        }
    }
}

private struct PersonCard: View {
    let person: Person
    let watched: Bool
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(spacing: 8) {
            PersonPicture(person: person, size: 72)
                .padding(4)
                .overlay {
                    if person.speaking {
                        Circle().strokeBorder(.good, lineWidth: 3)
                    }
                }
            HStack(spacing: 4) {
                Text(person.name).lineLimit(1)
                MutedIcon(person: person).font(.footnote)
            }
            .foregroundStyle(person.blocked ? ThemeColor.muted : .text)
            // Held empty for those not sharing, so every card has the same height.
            if let share = person.sharing, !person.blocked {
                Button {
                    model.core.watchOnly(user: person.user)
                } label: {
                    Label(watched ? "Watching" : "Watch", systemImage: "rectangle.on.rectangle")
                        .font(.caption.weight(.medium))
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(watched)
                .frame(height: 28)
                .accessibilityLabel("Watch \(person.name)’s share")
                .accessibilityValue(share.label)
            } else {
                Color.clear.frame(height: 28)
            }
        }
        .frame(maxWidth: .infinity, minHeight: 140)
        .padding(8)
        .background(person.blocked ? ThemeColor.bg : .panel, in: .rect(cornerRadius: 16))
        .overlay {
            if person.blocked {
                RoundedRectangle(cornerRadius: 16).strokeBorder(.panel)
            }
        }
    }
}

struct MicButton: View {
    /// The colour the button takes while on: the theme's text, or white over a share.
    var onColor: Color?
    @Environment(AppModel.self) private var model

    var body: some View {
        let on = model.state.mic && !model.micDenied
        Button("Microphone", systemImage: on ? "mic.fill" : "mic.slash.fill") {
            if model.micDenied {
                model.notice = "To talk, allow the microphone for throcc in Settings."
            } else {
                model.core.command(command: .setMic(on: !model.state.mic))
            }
        }
        .tint(on ? onColor ?? model.theme.text : model.theme.bad)
        .accessibilityValue(on ? "On" : "Off")
    }
}

struct SoundButton: View {
    /// The colour the button takes while on: the theme's text, or white over a share.
    var onColor: Color?
    @Environment(AppModel.self) private var model

    var body: some View {
        let deafened = model.state.deafened
        Button("Sound", systemImage: deafened ? "speaker.slash.fill" : "speaker.wave.2.fill") {
            model.core.command(command: .setDeafened(on: !deafened))
        }
        .tint(deafened ? model.theme.bad : onColor ?? model.theme.text)
        .accessibilityValue(deafened ? "Off" : "On")
    }
}

struct LeaveButton: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        Button("Leave Room", systemImage: "phone.down.fill") {
            model.core.command(command: .join(room: nil))
        }
        .tint(model.theme.bad)
    }
}
