import SwiftUI

@main
struct ThroccApp: App {
    init() {
        do {
            try setAppDirs()
        } catch {
            fatalError("no directories to keep throcc's data in: \(error)")
        }
    }

    var body: some Scene {
        WindowGroup {
            IdentityView()
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
