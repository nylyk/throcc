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
            let sharing = people.filter { !$0.blocked && $0.sharing != nil }
            ScrollView {
                if !sharing.isEmpty {
                    VStack(spacing: 12) {
                        ForEach(sharing, id: \.user) { person in
                            if let share = person.sharing {
                                ShareTile(person: person, share: share)
                            }
                        }
                    }
                    .padding([.horizontal, .top], 16)
                }
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 150), spacing: 12)], spacing: 12) {
                    ForEach(people, id: \.user) { person in
                        Button { model.sheet = .person(person.user) } label: { PersonCard(person: person) }
                            .buttonStyle(.plain)
                            .accessibilityValue(person.speaking ? "Speaking" : "")
                    }
                }
                .padding(16)
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
            Text(person.sharing.map { "Sharing \($0.label)" } ?? " ")
                .font(.caption)
                .foregroundStyle(.muted)
                .lineLimit(1)
                .accessibilityHidden(person.sharing == nil)
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
        .tint(on ? model.theme.text : model.theme.bad)
        .accessibilityValue(on ? "On" : "Off")
    }
}

struct SoundButton: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let deafened = model.state.deafened
        Button("Sound", systemImage: deafened ? "speaker.slash.fill" : "speaker.wave.2.fill") {
            model.core.command(command: .setDeafened(on: !deafened))
        }
        .tint(deafened ? model.theme.bad : model.theme.text)
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
