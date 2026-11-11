import SwiftUI

struct ServerPageView: View {
    let page: ServerPage

    var body: some View {
        Group {
            switch page {
            case .overview: ServerOverview()
            case .members: MembersPage()
            case .invites: InvitesPage()
            case .keys: KeysPage()
            }
        }
        .navigationBarTitleDisplayMode(.inline)
        .toolbar { CloseButton() }
    }
}

private struct ServerOverview: View {
    @Environment(AppModel.self) private var model
    @State private var name = ""
    @State private var picture: Data?
    /// The picture the server had when this page loaded or last saved it.
    @State private var keptPicture: Data?

    var body: some View {
        if let server = model.state.server {
            let editable = model.state.permissions?.editServer == true
            let check = checkName(typed: name)
            let changed = check.name != server.name || picture != keptPicture
            Form {
                Section {
                    if editable {
                        PictureEditor(picture: $picture, name: server.title)
                        TextField("Name", text: $name)
                    } else {
                        LabeledContent("Name") {
                            Avatar(picture: model.picture(in: picture), name: server.title, size: 28)
                            Text(server.title)
                        }
                    }
                    LabeledContent("Address", value: server.address)
                } footer: {
                    if let problem = check.problem {
                        Text(problem).foregroundStyle(.red)
                    }
                }
                Section("Fingerprint") {
                    Text(fingerprintText(key: server.fingerprint))
                        .font(.footnote)
                        .monospaced()
                        .textSelection(.enabled)
                }
                if editable {
                    Section {
                        Button("Save") { save(name: check.name, over: server) }
                            .disabled(!changed || check.problem != nil || model.serverSave == .saving)
                    } footer: {
                        SaveStatusText(status: model.serverSave)
                    }
                }
            }
            .navigationTitle("Overview")
            .onAppear {
                name = server.name ?? ""
                picture = server.avatar.flatMap { model.core.avatar(hash: $0) }
                keptPicture = picture
            }
        }
    }

    private func save(name: String?, over server: Server) {
        let name: NameChange = name == server.name ? .keep : .set(name: name)
        let picture: PictureChange = picture == keptPicture ? .keep : .set(picture: picture)
        model.core.command(command: .setServerProfile(name: name, picture: picture))
        keptPicture = self.picture
        model.serverSave = .saving
    }
}

private struct MembersPage: View {
    @Environment(AppModel.self) private var model
    @State private var removal: Removal?
    @State private var confirmsRemoval = false

    var body: some View {
        let state = model.state
        List {
            Section("\(model.users.count) People") {
                ForEach(model.users, id: \.id) { user in
                    let editable = user.id != state.me && state.role.map { canRemove(actor: $0, target: user.role) } == true
                    HStack(spacing: 12) {
                        Avatar(
                            picture: state.person(user.id)?.avatar.flatMap { model.pictures[$0] },
                            name: user.name,
                            size: 28
                        )
                        VStack(alignment: .leading, spacing: 2) {
                            Text(user.id == state.me ? "\(user.name) (you)" : user.name)
                            if let inviter = model.users.first(where: { $0.id == user.invitedBy }) {
                                Text("Invited by \(inviter.name)")
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                        }
                        Spacer()
                        role(of: user, editable: editable)
                    }
                    .swipeActions {
                        if editable {
                            Button("Remove") {
                                removal = Removal(user: user.id, name: user.name)
                                confirmsRemoval = true
                            }
                            .tint(.red)
                        }
                    }
                }
            }
        }
        .navigationTitle("Members")
        .onAppear { model.core.command(command: .listUsers) }
        .modifier(RemovalConfirmation(removal: removal, isPresented: $confirmsRemoval))
    }

    @ViewBuilder
    private func role(of user: User, editable: Bool) -> some View {
        let roles = [Role.manager, .user].filter { new in
            new == user.role || model.state.role.map { canSetRole(actor: $0, target: user.role, new: new) } == true
        }
        if editable, roles.count > 1 {
            Picker("Role", selection: Binding { user.role } set: { model.core.command(command: .setRole(user: user.id, role: $0)) }) {
                ForEach(roles, id: \.self) { Text($0.title) }
            }
            .labelsHidden()
        } else {
            Text(user.role.title).foregroundStyle(.secondary)
        }
    }
}

private struct InvitesPage: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        Form {
            Section {
                if let code = model.invite {
                    Text(code)
                        .font(.largeTitle)
                        .monospaced()
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity)
                    Button("Copy", systemImage: "doc.on.doc") { UIPasteboard.general.string = code }
                    ShareLink(item: code)
                }
                Button(model.invite == nil ? "Create Code" : "New Code") {
                    model.core.command(command: .createInvite)
                }
            } footer: {
                if let cap = model.state.permissions?.inviteCap {
                    Text("Single use, expires in 24 hours. You can make \(cap) a day.")
                } else {
                    Text("Single use, expires in 24 hours.")
                }
            }
        }
        .navigationTitle("Invites")
    }
}

private struct KeysPage: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let state = model.state
        List {
            if let server = state.server {
                Section("This Server") {
                    Text(server.address)
                    Text(fingerprintText(key: server.fingerprint))
                        .font(.footnote)
                        .monospaced()
                        .foregroundStyle(.secondary)
                        .textSelection(.enabled)
                }
            }
            Section("People Here") {
                ForEach(state.people.filter { $0.user != state.me }, id: \.user) { person in
                    NavigationLink {
                        PersonView(user: person.user)
                    } label: {
                        HStack(spacing: 12) {
                            PersonPicture(person: person, size: 28)
                            VStack(alignment: .leading, spacing: 2) {
                                Text(person.name)
                                if let key = person.identityKey {
                                    ShortKey(key: key)
                                }
                            }
                            Spacer()
                            TrustLabel(trust: state.trust(of: person)).font(.caption)
                        }
                    }
                }
            }
        }
        .navigationTitle("Keys")
    }
}
