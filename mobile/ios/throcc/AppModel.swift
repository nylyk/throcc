import SwiftUI

enum Sheet: Hashable, Identifiable {
    case addServer
    case settings
    case newRoom
    case editRoom(Room)
    case person(UInt64)
    case server(ServerPage)

    var id: Self { self }
}

enum ServerPage {
    case overview
    case members
    case invites
    case keys
}

enum SettingsPage {
    case profile
    case appearance
    case verified
    case blocked
    case about
}

enum SaveStatus: Equatable {
    case saving
    case saved
    case refused(String)
}

/// The state every screen shows: what the core keeps, the connection, and what is open.
@MainActor @Observable
final class AppModel {
    let core: Core
    private(set) var profile: Profile?
    private(set) var accent: Accent
    private(set) var theme: Theme
    private(set) var servers: [KnownServer] = []
    private(set) var state: CoreState
    /// Decoded pictures by the hash the state names them with.
    private(set) var pictures: [Data: UIImage] = [:]
    private(set) var users: [User] = []
    private(set) var invite: String?
    private(set) var connecting: String?
    private(set) var connectedAddress: String?
    private(set) var alert = ""
    var showsAlert = false
    var notice: String?
    var serverSave: SaveStatus?
    var passwordRefusal: String?
    var showsRooms = false
    var column = NavigationSplitViewColumn.sidebar
    var sheet: Sheet?
    var settingsPath: [SettingsPage] = []
    /// The connection that the samples of a lost connection show in place of the state's.
    var sampleConnection: Connection?
    @ObservationIgnored private let decoded = NSCache<NSData, UIImage>()

    init(core: Core) {
        self.core = core
        profile = core.profile()
        accent = core.accent()
        theme = Theme(core.palette())
        state = core.state()
        loadServers()
        core.setListener(listener: self)
    }

    var tint: Color { Color(rgb: accent.rgb) }

    /// Runs `action`, and shows why it failed as a notice.
    func attempt(_ action: () throws -> Void) {
        do {
            try action()
        } catch {
            notice = error.explanation
        }
    }

    func setProfile(name: String, picture: Data?) throws {
        try core.setProfile(profile: Profile(name: name, picture: picture))
        profile = core.profile()
    }

    func useAccent(named name: String) {
        attempt { try core.setAccent(name: name) }
        accent = core.accent()
        theme = Theme(core.palette())
    }

    func useAccent(rgb: UInt32) {
        attempt { try core.setCustomAccent(rgb: rgb) }
        accent = core.accent()
        theme = Theme(core.palette())
    }

    /// Signs in to the server at `address`, joining it first with the invite `code`, and opens
    /// its rooms.
    func connect(to address: String, code: String? = nil) async throws {
        if address == connectedAddress, state.connection == .connected {
            showsRooms = true
            return
        }
        connecting = address
        defer { connecting = nil }
        let core = core
        try await Task.detached(priority: .userInitiated) { try core.connect(address: address, invite: code) }.value
        connectedAddress = address
        users = []
        invite = nil
        serverSave = nil
        loadServers()
        showsRooms = true
    }

    /// Connects to a server on the list, and says why if that fails.
    func open(_ address: String) {
        Task {
            do {
                try await connect(to: address)
            } catch {
                alert = error.explanation
                showsAlert = true
            }
        }
    }

    func disconnect() {
        core.disconnect()
        connectedAddress = nil
        showsRooms = false
        sheet = nil
        loadServers()
    }

    func forget(_ address: String) {
        attempt { try core.forgetServer(address: address) }
        if address == connectedAddress {
            connectedAddress = nil
        }
        loadServers()
    }

    /// The picture in `bytes`, decoded once.
    func picture(in bytes: Data?) -> UIImage? {
        guard let bytes else { return nil }
        if let picture = decoded.object(forKey: bytes as NSData) {
            return picture
        }
        guard let picture = UIImage(data: bytes) else { return nil }
        decoded.setObject(picture, forKey: bytes as NSData)
        return picture
    }

    private func loadServers() {
        attempt { servers = try core.servers() }
    }

    private func apply(_ new: CoreState) {
        if new.current == nil {
            column = .sidebar
        } else if new.current?.room != state.current?.room {
            column = .detail
        }
        state = new
        if let sampleConnection {
            state.connection = sampleConnection
        }
        let hashes = state.people.compactMap(\.avatar) + [state.server?.avatar].compactMap { $0 }
        for hash in hashes where pictures[hash] == nil {
            loadPicture(hash)
        }
    }

    private func loadPicture(_ hash: Data) {
        pictures[hash] = core.avatar(hash: hash).flatMap { UIImage(data: $0) }
    }

    private func handle(_ event: Event) {
        switch event {
        case .invite(let code, _):
            invite = code
        case .users(let users):
            self.users = users
        case .notice(let text):
            notice = text
        case .removedFromServer(let reason):
            alert = reason
            showsAlert = true
            disconnect()
        case .avatarReady(let hash):
            loadPicture(hash)
        case .profileSaved:
            break
        case .profileRefused(let reason):
            notice = reason
        case .serverProfileSaved:
            serverSave = .saved
        case .serverProfileRefused(let reason):
            serverSave = .refused(reason)
        case .passwordRefused(let reason):
            passwordRefusal = reason
        case .leftServer:
            disconnect()
        }
    }
}

extension AppModel: Listener {
    nonisolated func state(state: CoreState) {
        DispatchQueue.main.async { self.apply(state) }
    }

    nonisolated func event(event: Event) {
        DispatchQueue.main.async { self.handle(event) }
    }
}

extension CoreState {
    func person(_ user: UInt64) -> Person? {
        people.first { $0.user == user }
    }

    var permissions: Permissions? {
        role.map(permissionsOf(role:))
    }
}
