import SwiftUI

struct RoomsView: View {
    @Environment(AppModel.self) private var model
    @State private var lockedRoom: Room?
    @State private var asksPassword = false
    @State private var password = ""
    @State private var deleted: Room?
    @State private var confirmsLeave = false

    var body: some View {
        let state = model.state
        let permissions = state.permissions
        let managesRooms = permissions?.manageRooms == true
        ThemedList {
            LostConnectionSection()
            ForEach(state.rooms, id: \.id) { room in
                Section {
                    Button { enter(room) } label: { label(for: room) }
                        .swipeActions {
                            if managesRooms {
                                Button("Delete", systemImage: "trash") { deleted = room }
                                    .tint(.red)
                                Button("Edit", systemImage: "pencil") { model.sheet = .editRoom(room) }
                            }
                        }
                    people(in: room.id)
                }
            }
            if state.rooms.isEmpty, state.connection == .connected {
                Text("No rooms yet.").foregroundStyle(.muted)
                if managesRooms {
                    Button("New Room") { model.sheet = .newRoom }
                }
            }
        }
        .navigationTitle(state.server?.title ?? "")
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Menu("Server", systemImage: "ellipsis") {
                    if managesRooms {
                        Button("New Room", systemImage: "plus") { model.sheet = .newRoom }
                        Divider()
                    }
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
                .tint(model.theme.text)
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
            isPresented: $deleted.isPresent(),
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
                .foregroundStyle(.muted)
                .frame(width: 28)
                .accessibilityLabel("Locked")
                .accessibilityHidden(!room.locked)
            Text(room.name)
                .font(.headline)
                .foregroundStyle(.text)
            Spacer()
            if model.state.current?.room == room.id {
                Image(systemName: "checkmark")
                    .foregroundStyle(.tint)
                    .accessibilityLabel("Joined")
            }
        }
    }

    private func people(in room: UInt64) -> some View {
        ForEach(model.state.people.filter { $0.room == room }, id: \.user) { person in
            Button { model.sheet = .person(person.user) } label: { PersonRow(person: person) }
        }
    }

    /// Joins `room`, asking for its password first where it needs one, or shows it where we
    /// are in it.
    private func enter(_ room: Room) {
        switch room.join {
        case .alreadyIn:
            model.column = .detail
        case .askPassword:
            model.passwordRefusal = nil
            password = ""
            lockedRoom = room
            asksPassword = true
        case .join:
            model.core.command(command: .join(room: room.id))
        }
    }
}

private struct LostConnectionSection: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        if let notice = model.state.notice {
            Section {
                if notice.busy {
                    Label { Text(notice.hint) } icon: { ProgressView() }
                        .foregroundStyle(.text)
                } else {
                    Text(notice.hint).foregroundStyle(.text)
                }
                if let problem = notice.problem {
                    Text(problem).foregroundStyle(.muted)
                }
                if let action = notice.action {
                    button(for: action)
                }
            }
        } else if model.state.connection == .disconnected, model.connecting != nil {
            Section { ProgressView() }
        }
    }

    @ViewBuilder
    private func button(for action: NoticeAction) -> some View {
        switch action {
        case .reconnect:
            Button("Reconnect") { model.core.command(command: .reconnect) }
        case .signInHere:
            connectAgain(titled: "Sign In Here")
        case .tryAgain:
            connectAgain(titled: "Try Again")
        }
    }

    @ViewBuilder
    private func connectAgain(titled title: LocalizedStringKey) -> some View {
        if model.connecting != nil {
            ProgressView()
        } else {
            Button(title) {
                if let address = model.state.server?.address {
                    model.open(address)
                }
            }
        }
    }
}

private struct PersonRow: View {
    @Environment(AppModel.self) private var model
    let person: Person

    var body: some View {
        HStack(spacing: 12) {
            PersonPicture(person: person, size: 28)
            Text(person.name)
                .foregroundStyle(person.blocked ? ThemeColor.muted : .text)
                .lineLimit(1)
            if person.trust != .verified, !person.blocked {
                TrustLabel(trust: person.trust).font(.caption)
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
            .foregroundStyle(.muted)
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
    @Environment(\.theme) private var theme
    let room: Room?
    @State private var name = ""
    @State private var password = ""

    var body: some View {
        let check = checkName(typed: name)
        let refusedPassword = password.isEmpty ? nil : passwordProblem(typed: password)
        NavigationStack {
            ThemedForm {
                Section {
                    ThemedField("Name", text: $name)
                } footer: {
                    if let problem = nameProblem(typed: name) {
                        Text(problem).foregroundStyle(.bad)
                    }
                }
                Section {
                    ThemedField("Password", text: $password, isSecret: true)
                    if let room, room.locked {
                        DestructiveButton("Remove Password") {
                            model.core.command(command: .setRoomPassword(room: room.id, password: nil))
                            dismiss()
                        }
                    }
                } footer: {
                    if let refusedPassword {
                        Text(refusedPassword).foregroundStyle(.bad)
                    } else {
                        Text(passwordHint).foregroundStyle(.muted)
                    }
                }
            }
            .navigationTitle(room == nil ? "New Room" : "Edit Room")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(role: .cancel) { dismiss() }
                        .tint(theme.text)
                }
                ToolbarItem(placement: .confirmationAction) {
                    ConfirmButton(room == nil ? "Create" : "Save") {
                        save(name: check.name)
                        dismiss()
                    }
                    .disabled(refusedPassword != nil || check.name == nil)
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
