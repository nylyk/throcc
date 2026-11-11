import Foundation

/// A screen that `--mock --screen <name>` opens directly.
enum Sample: String {
    case onboarding, servers, addserver, rooms, room, blocked, person, personblocked
    case profile, settings, appearance, verified, blockedpeople
    case server, members, invites, keys
    case reconnect, elsewhere
}

extension AppModel {
    func show(_ sample: Sample) async {
        switch sample {
        case .onboarding, .servers:
            break
        case .addserver:
            sheet = .addServer
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
        case .appearance:
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
