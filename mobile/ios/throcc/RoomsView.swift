import SwiftUI

struct RoomsView: View {
    @Environment(AppModel.self) private var model
    @State private var lockedRoom: Room?
    @State private var asksPassword = false
    @State private var password = ""
    @State private var deleted: Room?
    @State private var confirmsDelete = false
    @State private var confirmsLeave = false

    var body: some View {
        let state = model.state
        let permissions = state.permissions
        let managesRooms = permissions?.manageRooms == true
        List {
            LostConnectionSection()
            ForEach(state.rooms, id: \.id) { room in
                Section {
                    Button { enter(room, unasked: managesRooms) } label: { label(for: room) }
                        .swipeActions {
                            if managesRooms {
                                Button("Delete") {
                                    deleted = room
                                    confirmsDelete = true
                                }
                                .tint(.red)
                                Button("Edit") { model.sheet = .editRoom(room) }
                            }
                        }
                    people(in: room.id)
                }
            }
            if state.rooms.isEmpty, state.connection == .connected {
                Text("No rooms yet.").foregroundStyle(.secondary)
            }
            if state.people.contains(where: { $0.room == nil }) {
                Section("Lobby") { people(in: nil) }
            }
        }
        .navigationTitle(state.server?.title ?? "")
        .toolbar {
            if managesRooms {
                ToolbarItem(placement: .primaryAction) {
                    Button("New Room", systemImage: "plus") { model.sheet = .newRoom }
                }
            }
            ToolbarItem(placement: .primaryAction) {
                Menu("Server", systemImage: "ellipsis") {
                    Button("Overview", systemImage: "info.circle") { model.sheet = .server(.overview) }
                    if permissions?.listUsers == true {
                        Button("Members", systemImage: "person.2") { model.sheet = .server(.members) }
                    }
                    Button("Invites", systemImage: "person.badge.plus") { model.sheet = .server(.invites) }
                    Button("Keys", systemImage: "key") { model.sheet = .server(.keys) }
                    Divider()
                    Button("Disconnect", systemImage: "xmark.circle") { model.disconnect() }
                    Button("Leave Server", systemImage: "rectangle.portrait.and.arrow.right", role: .destructive) {
                        confirmsLeave = true
                    }
                }
            }
        }
        .alert("Join “\(lockedRoom?.name ?? "")”", isPresented: $asksPassword, presenting: lockedRoom) { room in
            SecureField("Password", text: $password)
            Button("Cancel", role: .cancel) {}
            Button("Join") {
                model.passwordRefusal = nil
                model.core.command(command: .joinWithPassword(room: room.id, password: password))
            }
            .disabled(password.isEmpty || !isValidPassword(typed: password))
        } message: { _ in
            Text(model.passwordRefusal ?? "This room is locked. Enter its password to join.")
        }
        .onChange(of: model.passwordRefusal) {
            if model.passwordRefusal != nil {
                password = ""
                asksPassword = true
            }
        }
        .confirmationDialog(
            "Delete “\(deleted?.name ?? "")”?",
            isPresented: $confirmsDelete,
            titleVisibility: .visible,
            presenting: deleted
        ) { room in
            Button("Delete", role: .destructive) { model.core.command(command: .deleteRoom(room: room.id)) }
        } message: { room in
            Text(
                state.people.contains { $0.room == room.id }
                    ? "Everyone in it goes back to the lobby. This can’t be undone."
                    : "This can’t be undone."
            )
        }
        .confirmationDialog(
            "Leave “\(state.server?.title ?? "")”?",
            isPresented: $confirmsLeave,
            titleVisibility: .visible
        ) {
            Button("Leave Server", role: .destructive) { model.core.command(command: .leaveServer) }
        } message: {
            Text("You lose your membership and the server leaves your list. Coming back needs a new invite.")
        }
    }

    private func label(for room: Room) -> some View {
        HStack {
            Image(systemName: room.locked ? "lock.fill" : "speaker.wave.2.fill")
                .foregroundStyle(Color.secondary)
                .frame(width: 28)
            Text(room.name)
                .font(.headline)
                .foregroundStyle(Color.primary)
            Spacer()
            if model.state.current?.room == room.id {
                Image(systemName: "checkmark")
            }
        }
    }

    private func people(in room: UInt64?) -> some View {
        ForEach(model.state.people.filter { $0.room == room }, id: \.user) { person in
            Button { model.sheet = .person(person.user) } label: { PersonRow(person: person) }
        }
    }

    /// Joins `room` or, where we are in it, shows it. A locked room asks for its password
    /// first unless we join it `unasked`.
    private func enter(_ room: Room, unasked: Bool) {
        if model.state.current?.room == room.id {
            model.column = .detail
        } else if room.locked && !unasked {
            model.passwordRefusal = nil
            password = ""
            lockedRoom = room
            asksPassword = true
        } else {
            model.core.command(command: .join(room: room.id))
        }
    }
}

private struct LostConnectionSection: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        switch model.state.connection {
        case .connected, .disconnected:
            EmptyView()
        case .reconnecting(let attempt, let reason):
            Section {
                Label {
                    Text("Reconnecting, attempt \(attempt). You’ll be put back in your room.")
                } icon: {
                    ProgressView()
                }
                Text("Lost the connection: \(reason).").foregroundStyle(.secondary)
            }
        case .awaitingReconnect:
            Section {
                Text("The connection was down for a while. Reconnecting here signs you out on any other device.")
                Button("Reconnect") { model.core.command(command: .reconnect) }
            }
        case .signedInElsewhere:
            Section {
                Text("Signed in on another device.")
                Button("Sign In Here", action: connectAgain)
            }
        case .failed(let reason):
            Section {
                Text(reason)
                Button("Try Again", action: connectAgain)
            }
        }
    }

    private func connectAgain() {
        if let address = model.connectedAddress {
            model.open(address)
        }
    }
}

private struct PersonRow: View {
    @Environment(AppModel.self) private var model
    let person: Person

    var body: some View {
        let trust = model.state.trust(of: person)
        HStack(spacing: 12) {
            PersonPicture(person: person, size: 28)
            Text(person.name)
                .foregroundStyle(person.blocked ? Color.secondary : .primary)
                .lineLimit(1)
            if trust != .verified, !person.blocked {
                TrustLabel(trust: trust).font(.caption)
            }
            Spacer()
            Group {
                if person.sharing != nil {
                    Image(systemName: "rectangle.inset.filled.and.person.filled")
                        .accessibilityLabel("Sharing")
                }
                if person.room != nil {
                    MutedIcon(person: person)
                }
            }
            .foregroundStyle(Color.secondary)
        }
    }
}

struct MutedIcon: View {
    let person: Person

    var body: some View {
        if person.deafened {
            Image(systemName: "speaker.slash.fill").accessibilityLabel("Deafened")
        } else if !person.mic {
            Image(systemName: "mic.slash.fill").accessibilityLabel("Microphone off")
        }
    }
}

struct RoomEditor: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let room: Room?
    @State private var name = ""
    @State private var password = ""

    var body: some View {
        let check = checkName(typed: name)
        let passwordFits = password.isEmpty || isValidPassword(typed: password)
        NavigationStack {
            Form {
                Section {
                    TextField("Name", text: $name)
                } footer: {
                    if let problem = check.problem {
                        Text(problem).foregroundStyle(.red)
                    }
                }
                Section {
                    SecureField("Password", text: $password)
                    if let room, room.locked {
                        Button("Remove Password", role: .destructive) {
                            model.core.command(command: .setRoomPassword(room: room.id, password: nil))
                            dismiss()
                        }
                    }
                } footer: {
                    if passwordFits {
                        Text(passwordHint)
                    } else {
                        Text("Passwords can be at most \(limits().passwordChars) characters.").foregroundStyle(.red)
                    }
                }
            }
            .navigationTitle(room == nil ? "New Room" : "Edit Room")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(role: .cancel) { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button(room == nil ? "Create" : "Save") {
                        save(name: check.name)
                        dismiss()
                    }
                    .disabled(!passwordFits || check.problem != nil || (room == nil && check.name == nil))
                }
            }
            .onAppear { name = room?.name ?? "" }
        }
    }

    private var passwordHint: String {
        switch room?.locked {
        case nil: "Leave it empty to let anyone join."
        case true?: "Members need the password to join. Type a new one to change it."
        case false?: "Anyone can join. Type a password to lock the room."
        }
    }

    private func save(name: String?) {
        let password = password.isEmpty ? nil : password
        guard let room else {
            if let name {
                model.core.command(command: .createRoom(name: name, password: password))
            }
            return
        }
        if let name, name != room.name {
            model.core.command(command: .renameRoom(room: room.id, name: name))
        }
        if let password {
            model.core.command(command: .setRoomPassword(room: room.id, password: password))
        }
    }
}
