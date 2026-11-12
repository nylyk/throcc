import AVFAudio

/// Runs the sound of the room we are in: the audio session, and the core's audio inside it.
@MainActor
final class CallAudio {
    private let core: Core
    private let showFailure: (String) -> Void
    private var inRoom = false
    private var interrupted = false
    /// Whether the session is set up for a room, also where the audio in it failed to open.
    private var started = false
    private var withMic = false
    /// The last of the steps, which run one after another in the order asked.
    private var lastStep: Task<Void, Never>?

    var micDenied: Bool {
        AVAudioApplication.shared.recordPermission == .denied
    }

    init(core: Core, showFailure: @escaping (String) -> Void) {
        self.core = core
        self.showFailure = showFailure
        let center = NotificationCenter.default
        center.addObserver(forName: AVAudioSession.interruptionNotification, object: nil, queue: nil) { [weak self] note in
            let type = (note.userInfo?[AVAudioSessionInterruptionTypeKey] as? UInt)
                .flatMap(AVAudioSession.InterruptionType.init(rawValue:))
            let options = AVAudioSession.InterruptionOptions(
                rawValue: note.userInfo?[AVAudioSessionInterruptionOptionKey] as? UInt ?? 0
            )
            DispatchQueue.main.async { self?.interruptionChanged(type, options) }
        }
        center.addObserver(forName: AVAudioSession.routeChangeNotification, object: nil, queue: nil) { [weak self] note in
            let reason = (note.userInfo?[AVAudioSessionRouteChangeReasonKey] as? UInt)
                .flatMap(AVAudioSession.RouteChangeReason.init(rawValue:))
            DispatchQueue.main.async { self?.routeChanged(reason) }
        }
        center.addObserver(forName: AVAudioSession.mediaServicesWereResetNotification, object: nil, queue: nil) { [weak self] _ in
            DispatchQueue.main.async { self?.mediaServicesWereReset() }
        }
    }

    /// Opens the audio once we are in a room, and closes it once we are out of one.
    func follow(inRoom: Bool) {
        guard inRoom != self.inRoom else { return }
        self.inRoom = inRoom
        enqueue { await self.settle() }
    }

    private func interruptionChanged(_ type: AVAudioSession.InterruptionType?, _ options: AVAudioSession.InterruptionOptions) {
        switch type {
        case .began:
            interrupted = true
        case .ended where options.contains(.shouldResume):
            interrupted = false
        default:
            return
        }
        enqueue { await self.settle() }
    }

    private func routeChanged(_ reason: AVAudioSession.RouteChangeReason?) {
        guard reason == .newDeviceAvailable || reason == .oldDeviceUnavailable else { return }
        enqueue {
            if self.started {
                await self.reopen()
            }
        }
    }

    private func mediaServicesWereReset() {
        enqueue {
            if self.started {
                await self.start()
            }
        }
    }

    /// Runs `step` once every step asked for before it has finished.
    private func enqueue(_ step: @escaping @MainActor () async -> Void) {
        lastStep = Task { [lastStep] in
            await lastStep?.value
            await step()
        }
    }

    /// Opens or closes the audio to match the room we are in and the interruption over it.
    private func settle() async {
        if !inRoom {
            interrupted = false
        }
        let wanted = inRoom && !interrupted
        if wanted, !started {
            await start()
        } else if !wanted, started {
            await stop(deactivating: !inRoom)
        }
    }

    /// Sets the session up for the room, with the mic where the user allows it, and opens the audio.
    private func start() async {
        started = true
        let permission = AVAudioApplication.shared.recordPermission
        withMic = if permission == .undetermined {
            await AVAudioApplication.requestRecordPermission()
        } else {
            permission == .granted
        }
        if !withMic {
            core.command(command: .setMic(on: false))
        }
        await run { [core, withMic] in
            let session = AVAudioSession.sharedInstance()
            if withMic {
                try session.setCategory(.playAndRecord, mode: .voiceChat, options: [.allowBluetoothHFP, .defaultToSpeaker])
            } else {
                try session.setCategory(.playback)
            }
            try session.setActive(true)
            try core.openAudio(withMic: withMic)
        }
    }

    private func reopen() async {
        await run { [core, withMic] in
            core.closeAudio()
            try core.openAudio(withMic: withMic)
        }
    }

    private func stop(deactivating: Bool) async {
        started = false
        await run { [core] in
            core.closeAudio()
            if deactivating {
                try? AVAudioSession.sharedInstance().setActive(false, options: .notifyOthersOnDeactivation)
            }
        }
    }

    /// Runs `work` off the main actor, and shows why it failed.
    private func run(_ work: @escaping @Sendable () throws -> Void) async {
        do {
            try await withCheckedThrowingContinuation { (done: CheckedContinuation<Void, Error>) in
                DispatchQueue.global(qos: .userInitiated).async { done.resume(with: Result(catching: work)) }
            }
        } catch {
            showFailure("The room has no sound: \(error.explanation)")
        }
    }
}
