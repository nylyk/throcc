import SwiftUI

@main
struct ThroccApp: App {
    private let launch: Result<AppModel, Error>
    private let sample: Sample?

    init() {
        let arguments = ProcessInfo.processInfo.arguments
        let mock = arguments.contains("--mock")
        let sample = arguments.drop { $0 != "--screen" }.dropFirst().first.flatMap(Sample.init(rawValue:))
        self.sample = mock ? sample : nil
        launch = Result {
            try setAppDirs()
            return AppModel(core: mock ? .mock(onboarded: sample != .onboarding) : try Core())
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
