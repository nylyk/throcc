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
    @Environment(\.theme) private var theme
    @State private var name = ""
    @State private var picture: Data?
    /// The picture the server had when this page loaded or last saved it.
    @State private var keptPicture: Data?

    var body: some View {
        if let server = model.state.server {
            let editable = model.state.permissions?.editServer == true
            let check = checkName(typed: name)
            let changed = check.name != server.name || picture != keptPicture
            ThemedForm {
                Section {
                    if editable {
                        PictureEditor(picture: $picture, name: server.title)
                        TextField("Name", text: $name, prompt: Text("Name").foregroundStyle(theme.muted))
                            .foregroundStyle(.text)
                    } else {
                        LabeledContent("Name") {
                            Avatar(picture: model.picture(in: picture), name: server.title, size: 28)
                            Text(server.title).foregroundStyle(.muted)
                        }
                        .foregroundStyle(.text)
                    }
                    LabeledContent("Address") { Text(server.address).foregroundStyle(.muted) }
                        .foregroundStyle(.text)
                } footer: {
                    if let problem = check.problem {
                        Text(problem).foregroundStyle(.bad)
                    }
                }
                Section {
                    Text(fingerprintText(key: server.fingerprint))
                        .font(.footnote)
                        .monospaced()
                        .foregroundStyle(.text)
                        .textSelection(.enabled)
                } header: {
                    Text("Fingerprint").foregroundStyle(.muted)
                }
                if editable {
                    Section {
                        if model.serverSave == .saving {
                            ProgressView()
                        } else {
                            Button("Save") { save(name: check.name, over: server) }
                                .disabled(!changed || check.problem != nil)
                        }
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
        ThemedList {
            Section {
                if model.users.isEmpty {
                    ProgressView()
                }
                ForEach(model.users, id: \.id) { user in
                    let editable = user.id != state.me && state.role.map { canRemove(actor: $0, target: user.role) } == true
                    HStack(spacing: 12) {
                        Avatar(
                            picture: state.person(user.id)?.avatar.flatMap { model.pictures[$0] },
                            name: user.name,
                            size: 28
                        )
                        VStack(alignment: .leading, spacing: 2) {
                            Text(user.id == state.me ? "\(user.name) (you)" : user.name).foregroundStyle(.text)
                            if let inviter = model.users.first(where: { $0.id == user.invitedBy }) {
                                Text("Invited by \(inviter.name)")
                                    .font(.caption)
                                    .foregroundStyle(.muted)
                            }
                        }
                        Spacer()
                        role(of: user, editable: editable)
                    }
                    .swipeActions {
                        if editable {
                            Button("Remove", systemImage: "minus.circle") {
                                removal = Removal(user: user.id, name: user.name)
                                confirmsRemoval = true
                            }
                            .tint(.red)
                        }
                    }
                }
            } header: {
                Text(model.users.count == 1 ? "1 Person" : "\(model.users.count) People").foregroundStyle(.muted)
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
            Text(user.role.title).foregroundStyle(.muted)
        }
    }
}

private struct InvitesPage: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        ThemedForm {
            Section {
                if let code = model.invite {
                    Text(code)
                        .font(.largeTitle)
                        .monospaced()
                        .foregroundStyle(.text)
                        .lineLimit(1)
                        .minimumScaleFactor(0.5)
                        .speechSpellsOutCharacters()
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity)
                    Button {
                        UIPasteboard.general.string = code
                        model.notice = "Invite code copied."
                    } label: {
                        IconLabel("Copy", symbol: "doc.on.doc")
                    }
                    ShareLink(item: code) { IconLabel("Share…", symbol: "square.and.arrow.up") }
                }
                Button(model.invite == nil ? "Create Code" : "Create New Code") {
                    model.core.command(command: .createInvite)
                }
            } footer: {
                Group {
                    if let cap = model.state.permissions?.inviteCap {
                        Text("Single use, expires in 24 hours. You can make \(cap) a day.")
                    } else {
                        Text("Single use, expires in 24 hours.")
                    }
                }
                .foregroundStyle(.muted)
            }
        }
        .navigationTitle("Invites")
    }
}

private struct KeysPage: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        let state = model.state
        ThemedList {
            if let server = state.server {
                Section {
                    Text(server.address).foregroundStyle(.text)
                    Text(fingerprintText(key: server.fingerprint))
                        .font(.footnote)
                        .monospaced()
                        .foregroundStyle(.muted)
                        .textSelection(.enabled)
                } header: {
                    Text("This Server").foregroundStyle(.muted)
                }
            }
            Section {
                ForEach(state.people.filter { $0.user != state.me }, id: \.user) { person in
                    NavigationLink {
                        PersonView(user: person.user)
                    } label: {
                        HStack(spacing: 12) {
                            PersonPicture(person: person, size: 28)
                            VStack(alignment: .leading, spacing: 2) {
                                Text(person.name).foregroundStyle(person.blocked ? ThemeColor.muted : .text)
                                if let key = person.identityKey {
                                    ShortKey(key: key)
                                }
                            }
                            Spacer()
                            TrustLabel(trust: state.trust(of: person)).font(.caption)
                        }
                    }
                }
            } header: {
                Text("People Here").foregroundStyle(.muted)
            }
        }
        .navigationTitle("Keys")
    }
}
