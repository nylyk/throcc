import Foundation

/// A screen that `--mock --screen <name>` opens directly.
enum Sample: String {
    case onboarding, servers, addserver, rooms, room, blocked, person, personblocked
    case profile, voice, settings, appearance, customtheme, verified, blockedpeople
    case server, members, invites, keys
    case reconnect, elsewhere
    case share, sharefull
    case pinmismatch, identitymissing
}

extension AppModel {
    func show(_ sample: Sample) async {
        switch sample {
        case .onboarding, .servers:
            break
        case .addserver:
            sheet = .addServer(address: "")
        case .rooms:
            await connectToSample()
        case .room:
            await joinSampleRoom()
        case .blocked:
            await joinSampleRoom()
            core.command(command: .block(user: 3))
        case .person:
            await joinSampleRoom()
            sheet = .person(4)
        case .personblocked:
            await joinSampleRoom()
            core.command(command: .block(user: 3))
            sheet = .person(3)
        case .settings:
            await showSampleSettings([])
        case .profile:
            await showSampleSettings([.profile])
        case .voice:
            await showSampleSettings([.voice])
        case .appearance:
            await showSampleSettings([.appearance])
        case .customtheme:
            useCustomTheme(customPalette)
            await showSampleSettings([.appearance])
        case .verified:
            await showSampleSettings([.verified])
        case .blockedpeople:
            await showSampleSettings([.blocked])
            core.command(command: .block(user: 3))
        case .server:
            await showSample(.overview)
        case .members:
            await showSample(.members)
        case .invites:
            await showSample(.invites)
            core.command(command: .createInvite)
        case .keys:
            await showSample(.keys)
        case .reconnect:
            sampleConnection = .awaitingReconnect
            await connectToSample()
        case .elsewhere:
            sampleConnection = .signedInElsewhere
            await connectToSample()
        case .share:
            await joinSampleRoom()
            core.watchOnly(user: 4)
        case .sharefull:
            await joinSampleRoom()
            core.watchOnly(user: 4)
            // Once the mock shows the share watched, which fullscreen goes with.
            while !(state.current?.watching.contains(4) ?? false) {
                try? await Task.sleep(for: .milliseconds(20))
            }
            fullscreenShare = 4
        case .pinmismatch:
            sheet = .pinMismatch(
                address: "owls.thro.cc",
                expected: Data(repeating: 0x2c, count: 32),
                actual: Data(repeating: 0xb8, count: 32)
            )
        case .identitymissing:
            sheet = .identityMissing(address: "owls.thro.cc")
        }
    }

    private func connectToSample() async {
        try? await connect(to: "owls.thro.cc")
    }

    private func joinSampleRoom() async {
        await connectToSample()
        core.command(command: .join(room: 1))
    }

    private func showSampleSettings(_ path: [SettingsPage]) async {
        await connectToSample()
        settingsPath = path
        sheet = .settings
    }

    private func showSample(_ page: ServerPage) async {
        await connectToSample()
        sheet = .server(page)
    }
}
