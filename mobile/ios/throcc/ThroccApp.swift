import SwiftUI

@main
struct ThroccApp: App {
    @UIApplicationDelegateAdaptor private var delegate: AppDelegate
    private let launch: Result<AppModel, Error>
    private let sample: Sample?

    init() {
        let arguments = ProcessInfo.processInfo.arguments
        if arguments.contains("--software-decode") {
            setenv("THROCC_SOFTWARE_DECODE", "1", 1)
        }
        let mock = arguments.contains("--mock")
        let value = { (flag: String) in arguments.drop { $0 != flag }.dropFirst().first }
        let sample = value("--screen").flatMap(Sample.init(rawValue:))
        self.sample = mock ? sample : nil
        launch = Result {
            try setAppDirs()
            let model = AppModel(core: mock ? .mock(onboarded: sample != .onboarding) : Core(), mock: mock)
            if mock, let theme = value("--theme") {
                model.useTheme(named: theme)
            }
            return model
        }
    }

    var body: some Scene {
        WindowGroup {
            switch launch {
            case .success(let model):
                RootView(sample: sample)
                    .environment(model)
            case .failure(let error):
                ContentUnavailableView(
                    "throcc can’t start",
                    systemImage: "exclamationmark.triangle",
                    description: Text(error.explanation)
                )
            }
        }
    }
}

/// Tells the core where the app keeps its data and its cache, and keeps the data out of backups.
private func setAppDirs() throws {
    let files = FileManager.default
    var data = try files.url(for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: true)
    let cache = try files.url(for: .cachesDirectory, in: .userDomainMask, appropriateFor: nil, create: true)
    // A restored backup would roll the epoch floors back under the same identity key.
    var values = URLResourceValues()
    values.isExcludedFromBackup = true
    try data.setResourceValues(values)
    setAppDirs(data: data.path, cache: cache.path)
}

final class AppDelegate: NSObject, UIApplicationDelegate {
    func application(
        _ application: UIApplication,
        supportedInterfaceOrientationsFor window: UIWindow?
    ) -> UIInterfaceOrientationMask {
        Turning.allowed
    }
}

/// Which ways the window may turn: a phone stays upright except while a share fills it.
@MainActor
enum Turning {
    private static let phone = UIDevice.current.userInterfaceIdiom == .phone
    static private(set) var allowed: UIInterfaceOrientationMask = phone ? .portrait : .all
    /// Follows the phone's turns while the window may turn.
    private static var following: NSObjectProtocol?

    /// Lets a phone's window turn with the phone, or turns it back upright.
    static func allow(_ turning: Bool) {
        guard phone else { return }
        allowed = turning ? .allButUpsideDown : .portrait
        if let following {
            NotificationCenter.default.removeObserver(following)
        }
        following = nil
        if turning {
            UIDevice.current.beginGeneratingDeviceOrientationNotifications()
            following = NotificationCenter.default.addObserver(
                forName: UIDevice.orientationDidChangeNotification,
                object: nil,
                queue: .main
            ) { _ in
                MainActor.assumeIsolated { turn(to: deviceMask()) }
            }
        }
        turn(to: turning ? deviceMask() : .portrait)
    }

    /// The window orientation that matches how the phone is held, or `nil` when flat or upside down.
    private static func deviceMask() -> UIInterfaceOrientationMask? {
        // A device turned left shows its interface turned right.
        switch UIDevice.current.orientation {
        case .portrait: .portrait
        case .landscapeLeft: .landscapeRight
        case .landscapeRight: .landscapeLeft
        default: nil
        }
    }

    /// Updates what each window may show and turns it to `mask`, if given.
    private static func turn(to mask: UIInterfaceOrientationMask?) {
        for case let scene as UIWindowScene in UIApplication.shared.connectedScenes {
            var controller = scene.keyWindow?.rootViewController
            while let shown = controller {
                shown.setNeedsUpdateOfSupportedInterfaceOrientations()
                controller = shown.presentedViewController
            }
            if let mask {
                scene.requestGeometryUpdate(.iOS(interfaceOrientations: mask))
            }
        }
    }
}
