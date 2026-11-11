import SwiftUI

struct RoomView: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let state = model.state
        if let current = state.current, let room = state.rooms.first(where: { $0.id == current.room }) {
            ScrollView {
                LazyVGrid(columns: [GridItem(.adaptive(minimum: 150), spacing: 12)], spacing: 12) {
                    ForEach(state.people.filter { $0.room == room.id }, id: \.user) { person in
                        Button { model.sheet = .person(person.user) } label: { PersonCard(person: person) }
                            .buttonStyle(.plain)
                    }
                }
                .padding(16)
            }
            .background(.bg)
            .navigationTitle(room.name)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItemGroup(placement: .bottomBar) {
                    Button("Microphone", systemImage: state.mic ? "mic.fill" : "mic.slash.fill") {
                        model.core.command(command: .setMic(on: !state.mic))
                    }
                    .tint(state.mic ? .primary : model.theme.bad)
                    Button("Sound", systemImage: state.deafened ? "speaker.slash.fill" : "speaker.wave.2.fill") {
                        model.core.command(command: .setDeafened(on: !state.deafened))
                    }
                    .tint(state.deafened ? model.theme.bad : .primary)
                    Spacer()
                    Button("Leave Room", systemImage: "phone.down.fill") {
                        model.core.command(command: .join(room: nil))
                    }
                    .tint(.red)
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
            .foregroundStyle(person.blocked ? AnyShapeStyle(.muted) : AnyShapeStyle(.primary))
            if let sharing = person.sharing {
                Text("Sharing \(sharing.label)")
                    .font(.caption)
                    .foregroundStyle(.muted)
                    .lineLimit(1)
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
