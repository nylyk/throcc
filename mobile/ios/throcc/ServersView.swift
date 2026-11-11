import SwiftUI

struct ServersView: View {
    @Environment(AppModel.self) private var model
    @State private var removed: KnownServer?
    @State private var confirmsRemoval = false

    var body: some View {
        ThemedList {
            ForEach(model.servers, id: \.address) { server in
                Button { model.open(server.address) } label: { row(for: server) }
                    .swipeActions {
                        Button("Remove", systemImage: "minus.circle") {
                            removed = server
                            confirmsRemoval = true
                        }
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
                    Button { model.sheet = .addServer } label: { Text("Add Server").touchTarget() }
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
                Button("Add Server", systemImage: "plus") { model.sheet = .addServer }
                    .tint(model.theme.text)
            }
        }
        .confirmationDialog(
            "Remove “\(removed?.title ?? "")”?",
            isPresented: $confirmsRemoval,
            titleVisibility: .visible,
            presenting: removed
        ) { server in
            Button("Remove from List", role: .destructive) { model.forget(server.address) }
        } message: { _ in
            Text("It leaves your list, but you stay a member. Adding it again needs an invite. To end your membership, connect and choose Leave Server.")
        }
    }

    private func row(for server: KnownServer) -> some View {
        HStack(spacing: 12) {
            Avatar(picture: model.picture(in: server.picture), name: server.title, size: 40)
            VStack(alignment: .leading, spacing: 2) {
                Text(server.title).foregroundStyle(.text)
                if server.name != nil {
                    Text(server.address)
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
    @State private var address = ""
    @State private var code = ""
    @State private var problem: String?

    var body: some View {
        let address = address.trimmingCharacters(in: .whitespaces)
        let joinable = !address.isEmpty && (code.isEmpty || isInviteCode(typed: code))
        NavigationStack {
            ThemedForm {
                Section {
                    TextField("Address", text: $address, prompt: Text("Address").foregroundStyle(theme.muted))
                        .keyboardType(.URL)
                        .textInputAutocapitalization(.never)
                    TextField("Invite code", text: $code, prompt: Text("Invite code").foregroundStyle(theme.muted))
                        .monospaced()
                        .textInputAutocapitalization(.characters)
                } footer: {
                    Text("The code also proves you reached the real server. Leave it empty for a server you’ve joined before.")
                        .foregroundStyle(.muted)
                }
                .autocorrectionDisabled()
                .foregroundStyle(.text)
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
        }
    }

    private func join(_ address: String) {
        Task {
            do {
                try await model.connect(to: address, code: code.isEmpty ? nil : code)
                dismiss()
            } catch {
                problem = error.explanation
            }
        }
    }
}
