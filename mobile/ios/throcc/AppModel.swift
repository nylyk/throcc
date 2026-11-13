import SwiftUI

enum Sheet: Hashable, Identifiable {
    case addServer(address: String)
    case settings
    case newRoom
    case editRoom(Room)
    case person(UInt64)
    case server(ServerPage)
    /// The server at `address` showed another key than the one this install trusted for it,
    /// on a sign-in with the invite `code`, if any.
    case pinMismatch(address: String, code: String?, expected: Data, actual: Data)
    /// Signing in to the server at `address`, with the invite `code` if any, found our identity
    /// gone from the keychain.
    case identityMissing(address: String, code: String?)

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
    case voice
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

struct AlertText {
    let title: String
    let message: String
}

/// The state every screen shows: what the core keeps, the connection, and what is open.
@MainActor @Observable
final class AppModel {
    let core: Core
    /// Why what this install keeps couldn't be read, where it couldn't.
    let storeProblems: StoreProblems
    private(set) var profile: Profile?
    private(set) var voice: Voice
    let themes: [ThemeChoice]
    private(set) var pickedTheme: ThemeChoice
    /// The colours the Custom theme has, or starts from.
    private(set) var customPalette: Palette
    private(set) var theme: Theme
    private(set) var servers: [KnownServer] = []
    private(set) var state: CoreState
    /// Decoded pictures by the hash the state names them with.
    private(set) var pictures: [Data: UIImage] = [:]
    private(set) var users: [User] = []
    private(set) var invite: String?
    private(set) var connecting: String?
    private(set) var connectedAddress: String?
    var alert: AlertText?
    var notice: String? {
        didSet {
            if let notice {
                AccessibilityNotification.Announcement(notice).post()
            }
        }
    }
    var serverSave: SaveStatus?
    var passwordRefusal: String?
    var showsRooms = false
    var column = NavigationSplitViewColumn.sidebar
    var sheet: Sheet?
    var settingsPath: [SettingsPage] = []
    /// The connection that the samples of a lost connection show in place of the state's.
    var sampleConnection: Connection?
    /// The share shown over the whole screen, if any.
    var fullscreenShare: UInt64?
    @ObservationIgnored private(set) lazy var pictureInPicture = SharePictureInPicture(core: core)
    @ObservationIgnored private let decoded = NSCache<NSData, UIImage>()
    /// The hashes of the pictures the core was asked for on this connection.
    @ObservationIgnored private var askedPictures: Set<Data> = []
    /// The alert that waits for the sheet over the root to close.
    @ObservationIgnored private var heldAlert: AlertText?
    /// Runs the core's connects, disconnects and forgets off the main actor, in the order asked.
    @ObservationIgnored private let switching = DispatchQueue(label: "cc.thro.throcc.switching", qos: .userInitiated)
    /// The number of connects and disconnects asked for, by which a connect sees a later one.
    @ObservationIgnored private var switches = 0
    /// Runs the theme's saves off the main actor, in the order asked.
    @ObservationIgnored private let themeSaves = DispatchQueue(label: "cc.thro.throcc.theme-saves", qos: .utility)
    /// Waits for the Custom theme's colours to rest before it saves them.
    @ObservationIgnored private var customThemeSave: Task<Void, Never>?
    /// The room's sound, which only a real server has.
    @ObservationIgnored private var callAudio: CallAudio?

    /// Whether the system keeps the microphone from throcc.
    var micDenied: Bool { callAudio?.micDenied ?? false }

    init(core: Core, mock: Bool) {
        self.core = core
        storeProblems = core.storeProblems()
        profile = core.profile()
        voice = core.voice()
        themes = core.themes()
        let picked = core.theme()
        pickedTheme = picked
        customPalette = core.customPalette()
        theme = Theme(picked.palette)
        state = core.state()
        loadServers()
        core.setListener(listener: self)
        if !mock {
            callAudio = CallAudio(core: core) { [weak self] in self?.notice = $0 }
        }
    }

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

    /// Applies `voice` to the call at once, and keeps it when `keep`.
    func setVoice(_ voice: Voice, keep: Bool) {
        self.voice = voice
        attempt { try core.setVoice(voice: voice, keep: keep) }
    }

    func useTheme(named name: String) {
        saveCustomTheme()
        attempt { try themeSaves.sync { try core.setTheme(name: name) } }
        loadTheme()
    }

    /// Shows `palette` as the Custom theme at once, and saves it once its colours rest.
    func useCustomTheme(_ palette: Palette) {
        customPalette = palette
        pickedTheme = ThemeChoice(name: customThemeName(), palette: palette)
        theme = Theme(palette)
        customThemeSave?.cancel()
        customThemeSave = Task {
            if (try? await Task.sleep(for: .milliseconds(300))) != nil {
                saveCustomTheme()
            }
        }
    }

    /// Saves the Custom theme's colours, where a change left them unsaved.
    func saveCustomTheme() {
        guard let waiting = customThemeSave else { return }
        waiting.cancel()
        customThemeSave = nil
        let (core, palette) = (core, customPalette)
        themeSaves.async {
            do {
                try core.setCustomTheme(palette: palette)
            } catch {
                let problem = error.explanation
                DispatchQueue.main.async {
                    self.notice = problem
                    self.loadTheme()
                }
            }
        }
    }

    /// Signs in to the server at `address`, joining it first with the invite `code`, and opens
    /// its rooms. It does nothing while another sign-in is under way. A changed server key or a
    /// missing identity opens its dialog in place of the sheet that asked rather than throwing,
    /// and signing in closes the add-server sheet or either dialog.
    func connect(to address: String, code: String? = nil) async throws {
        if address == connectedAddress, state.holdsSession {
            showsRooms = true
            return
        }
        guard connecting == nil else { return }
        connecting = address
        defer { connecting = nil }
        switches += 1
        let (asked, core, switching) = (switches, core, switching)
        do {
            try await withCheckedThrowingContinuation { (done: CheckedContinuation<Void, Error>) in
                switching.async { done.resume(with: Result { try core.connect(address: address, invite: code) }) }
            }
        } catch {
            guard asked == switches else { return }
            connectedAddress = nil
            showsRooms = false
            switch error as? CoreError {
            case .PinMismatch(let expected, let actual):
                sheet = .pinMismatch(address: address, code: code, expected: expected, actual: actual)
            case .IdentityMissing:
                sheet = .identityMissing(address: address, code: code)
            case .Failed, nil:
                throw error
            }
            return
        }
        guard asked == switches else { return }
        connectedAddress = address
        users = []
        invite = nil
        loadServers()
        showsRooms = true
        switch sheet {
        case .addServer, .pinMismatch, .identityMissing:
            sheet = nil
        default:
            break
        }
    }

    /// Trusts `actual` as the key of the server at `address` on the user's word, and signs in
    /// to it again with the invite `code`, if any, as `connect(to:code:)` does.
    func trustServerKey(at address: String, actual: Data, code: String?) async throws {
        try core.trustServerKey(address: address, actual: actual)
        try await connect(to: address, code: code)
    }

    /// Makes a new identity and forgets every server, unless the keychain holds the old
    /// identity again by now. It runs off the main actor, as the keychain may ask the user.
    func startOver() async throws -> StartedOver {
        let (core, switching) = (core, switching)
        let started = try await withCheckedThrowingContinuation { (done: CheckedContinuation<StartedOver, Error>) in
            switching.async { done.resume(with: Result { try core.startOver() }) }
        }
        loadServers()
        return started
    }

    /// Connects to a server on the list, and says why if that fails.
    func open(_ address: String) {
        Task {
            do {
                try await connect(to: address)
            } catch {
                show(alert: error.explanation, titled: "Couldn’t Connect")
            }
        }
    }

    func disconnect() {
        switches += 1
        connectedAddress = nil
        showsRooms = false
        sheet = nil
        let core = core
        switching.async {
            core.disconnect()
            DispatchQueue.main.async { self.loadServers() }
        }
    }

    func forget(_ address: String) {
        if address == connectedAddress || address == connecting {
            switches += 1
            connectedAddress = nil
        }
        servers.removeAll { $0.address == address }
        let core = core
        switching.async {
            let problem: String?
            do {
                try core.forgetServer(address: address)
                problem = nil
            } catch {
                problem = error.explanation
            }
            DispatchQueue.main.async {
                if let problem {
                    self.notice = problem
                }
                self.loadServers()
            }
        }
    }

    /// The picture in `bytes`, decoded once.
    func picture(in bytes: Data?) -> UIImage? {
        guard let bytes else { return nil }
        if let picture = decoded.object(forKey: bytes as NSData) {
            return picture
        }
        guard let picture = shownPicture(in: bytes) else { return nil }
        decoded.setObject(picture, forKey: bytes as NSData)
        return picture
    }

    /// Resets what the closed sheet leaves behind, and shows the alert that waited for it.
    func sheetClosed() {
        settingsPath = []
        if let heldAlert {
            alert = heldAlert
            self.heldAlert = nil
        }
    }

    private func loadTheme() {
        pickedTheme = core.theme()
        customPalette = core.customPalette()
        theme = Theme(pickedTheme.palette)
    }

    private func show(alert message: String, titled title: String) {
        let alert = AlertText(title: title, message: message)
        if sheet == nil {
            self.alert = alert
        } else {
            heldAlert = alert
        }
    }

    private func loadServers() {
        servers = core.servers()
    }

    private func apply(_ new: CoreState) {
        if new.current == nil {
            column = .sidebar
        } else if new.current?.room != state.current?.room {
            column = .detail
        }
        state = new
        // Fullscreen ends with the share or the watching, also when the room or the server
        // goes away.
        if let user = fullscreenShare,
            !(new.current?.watching.contains(user) ?? false)
                || new.people.first(where: { $0.user == user })?.sharing == nil
        {
            fullscreenShare = nil
        }
        if let user = pictureInPicture.showing?.user,
            !(new.current?.watching.contains(user) ?? false)
                || new.people.first(where: { $0.user == user })?.sharing == nil
        {
            pictureInPicture.stop()
        }
        callAudio?.follow(inRoom: new.liveRoom != nil)
        if let sampleConnection {
            state.connection = sampleConnection
            state.notice = connectionNotice(connection: sampleConnection)
        }
        if state.connection != .connected {
            serverSave = nil
        }
        if state.server == nil {
            pictures = [:]
            askedPictures = []
        }
        let hashes = state.people.compactMap(\.avatar) + [state.server?.avatar].compactMap { $0 }
        for hash in hashes where askedPictures.insert(hash).inserted {
            loadPicture(hash)
        }
    }

    private func loadPicture(_ hash: Data) {
        pictures[hash] = core.avatar(hash: hash).flatMap(shownPicture(in:))
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
            show(alert: reason, titled: "Removed from Server")
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
            loadServers()
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
