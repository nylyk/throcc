import SwiftUI

struct ServersView: View {
    @Environment(AppModel.self) private var model
    @State private var removed: KnownServer?

    var body: some View {
        ThemedList {
            ForEach(model.servers, id: \.address) { server in
                Button { model.open(server.address) } label: { row(for: server) }
                    .swipeActions {
                        Button("Remove", systemImage: "minus.circle") { removed = server }
                            .tint(.red)
                    }
            }
        }
        .overlay {
            if model.servers.isEmpty {
                ContentUnavailableView {
                    Label("No Servers", systemImage: "server.rack").foregroundStyle(.text)
                } description: {
                    Text("Add your first server to get started.").foregroundStyle(.muted)
                } actions: {
                    Button { model.sheet = .addServer(address: "") } label: { Text("Add Server").touchTarget() }
                }
            }
        }
        .navigationTitle("Servers")
        .toolbar {
            ToolbarItem(placement: .topBarLeading) {
                Button("Settings", systemImage: "gearshape") { model.sheet = .settings }
                    .tint(model.theme.text)
            }
            ToolbarItem(placement: .primaryAction) {
                Button("Add Server", systemImage: "plus") { model.sheet = .addServer(address: "") }
                    .tint(model.theme.text)
            }
        }
        .confirmationDialog(
            "Remove “\(removed?.title ?? "")”?",
            isPresented: $removed.isPresent(),
            titleVisibility: .visible,
            presenting: removed
        ) { server in
            Button("Remove from List", role: .destructive) { model.forget(server.address) }
        } message: { _ in
            Text("The server is removed from your list, but you stay a member. Adding it again needs only its address. To end your membership, connect and choose Leave Server.")
        }
    }

    private func row(for server: KnownServer) -> some View {
        HStack(spacing: 12) {
            Avatar(picture: model.picture(in: server.picture), initial: initial(name: server.title), size: 40)
            VStack(alignment: .leading, spacing: 2) {
                Text(server.title).lineLimit(1).foregroundStyle(.text)
                if server.name != nil {
                    Text(server.address)
                        .lineLimit(1)
                        .font(.subheadline)
                        .foregroundStyle(.muted)
                }
            }
            Spacer()
            if model.connecting == server.address {
                ProgressView()
            } else if model.connectedAddress == server.address {
                Text("Connected")
                    .font(.caption)
                    .foregroundStyle(.muted)
            }
        }
    }
}

struct AddServerView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @Environment(\.theme) private var theme
    @State private var address: String
    @State private var code = ""
    @State private var problem: String?
    @State private var joining = false
    @State private var needsInvite: Bool

    init(address: String, needsInvite: Bool) {
        _address = State(initialValue: address)
        _needsInvite = State(initialValue: needsInvite)
    }

    var body: some View {
        let address = address.trimmingCharacters(in: .whitespaces)
        let joinable = !address.isEmpty && (!needsInvite || isInviteCode(typed: code))
        NavigationStack {
            ThemedForm {
                if needsInvite {
                    Section {
                        Text("Join \(address) with an invite code.")
                            .foregroundStyle(.text)
                        ThemedField("Invite code", text: $code)
                            .monospaced()
                            .textInputAutocapitalization(.characters)
                    }
                    .autocorrectionDisabled()
                } else {
                    Section {
                        ThemedField("Address", text: $address)
                            .keyboardType(.URL)
                            .textInputAutocapitalization(.never)
                    }
                    .autocorrectionDisabled()
                }
                if let problem {
                    Text(problem).foregroundStyle(.bad)
                }
            }
            .navigationTitle("Add Server")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(role: .cancel) { dismiss() }
                        .tint(theme.text)
                        .disabled(joining)
                }
                ToolbarItem(placement: .confirmationAction) {
                    if model.connecting == nil {
                        ConfirmButton("Join") { join(address) }
                            .disabled(!joinable)
                    } else {
                        ProgressView()
                    }
                }
            }
            .interactiveDismissDisabled(joining)
        }
    }

    private func join(_ address: String) {
        Task {
            joining = true
            defer { joining = false }
            do {
                // The model closes this sheet once joined.
                try await model.connect(to: address, code: needsInvite ? code : nil)
            } catch CoreError.NeedsInvite {
                needsInvite = true
                problem = nil
            } catch {
                problem = error.explanation
            }
        }
    }
}

/// The server at `address` showed another key than the one this install trusted for it.
struct PinMismatchView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.theme) private var theme
    let address: String
    let expected: Data
    let actual: Data

    var body: some View {
        NavigationStack {
            ThemedForm {
                Section {
                    key("You trusted", expected)
                    key("It now has", actual)
                }
                Section {
                    Text("This happens when the server was reinstalled, and when someone is pretending to be it.")
                        .foregroundStyle(.text)
                }
                Section {
                    Button("Use a New Invite") { model.sheet = .addServer(address: address, needsInvite: true) }
                }
            }
            .navigationTitle("This server’s key changed")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(role: .close) { model.sheet = nil }
                        .tint(theme.text)
                }
            }
        }
    }

    private func key(_ title: LocalizedStringKey, _ key: Data) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title)
                .font(.caption)
                .foregroundStyle(.muted)
            Text(fingerprintText(key: key))
                .font(.footnote)
                .monospaced()
                .foregroundStyle(.text)
                .textSelection(.enabled)
        }
    }
}

/// Signing in to the server at `address`, with the invite `code` if any, found our identity
/// gone from the keychain. Offers to look again, or to start over with a new one.
struct IdentityMissingView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.theme) private var theme
    let address: String
    let code: String?
    @State private var startingOver = false
    @State private var joining = false
    @State private var started: StartedOver?
    @State private var problem: String?

    var body: some View {
        NavigationStack {
            ThemedForm {
                switch started {
                case .recovered?:
                    Section {
                        Text("Your keychain had it after all. Your servers are still on your list.").foregroundStyle(.text)
                    }
                    if let problem {
                        Text(problem).foregroundStyle(.bad)
                    }
                    Section {
                        Button(joining ? "Connecting…" : "Join") { tryAgain() }
                            .disabled(joining)
                    }
                case .newIdentity?:
                    Section {
                        Text("Your servers have left your list. Add each one again with a new invite.").foregroundStyle(.text)
                    }
                case nil:
                    Section {
                        Text("Your identity is missing from this device’s keychain, so your servers won’t know it’s you. If it still isn’t found, restart throcc.")
                        Text("Starting over makes a new identity and takes every server off your list. Joining your servers again needs new invites, and anyone who verified you needs to verify you again.")
                    }
                    .foregroundStyle(.text)
                    if let problem {
                        Text(problem).foregroundStyle(.bad)
                    }
                    Section {
                        Button(joining ? "Connecting…" : "Try Again") { tryAgain() }
                            .disabled(busy)
                        DestructiveButton(startingOver ? "Starting Over…" : "Start Over") { startOver() }
                            .disabled(busy)
                    }
                }
            }
            .navigationTitle(title)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(role: started == nil ? .cancel : .close) { model.sheet = nil }
                        .tint(theme.text)
                        .disabled(busy)
                }
            }
            .interactiveDismissDisabled(busy)
        }
    }

    private var busy: Bool { startingOver || joining }

    private var title: String {
        switch started {
        case .recovered?: "Your identity is back"
        case .newIdentity?: "You have a new identity"
        case nil: "Your identity is missing"
        }
    }

    private func tryAgain() {
        Task {
            joining = true
            defer { joining = false }
            do {
                // The model closes this dialog once signed in, and keeps it up where the
                // identity is still missing.
                try await model.connect(to: address, code: code)
                if model.sheet == .identityMissing(address: address, code: code) {
                    problem = "Your identity still isn’t in the keychain."
                }
            } catch {
                problem = error.explanation
            }
        }
    }

    private func startOver() {
        Task {
            startingOver = true
            defer { startingOver = false }
            do {
                started = try await model.startOver()
                problem = nil
            } catch {
                problem = error.explanation
            }
        }
    }
}
