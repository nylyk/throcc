import AVFoundation
import AVKit
import SwiftUI
import UIKit

/// A view backed by a display layer, which the core enqueues a share's decoded pictures into,
/// uncopied.
final class SampleBufferView: UIView {
    override class var layerClass: AnyClass { AVSampleBufferDisplayLayer.self }

    var displayLayer: AVSampleBufferDisplayLayer {
        // The layer class above makes this one.
        layer as! AVSampleBufferDisplayLayer
    }
}

/// Where a share surface's view is, once made.
final class SurfaceHandle {
    weak var view: UIView?
}

/// `user`'s share, fitted into the view in its shape. `handle` learns the view.
struct ShareSurface: UIViewRepresentable {
    let user: UInt64
    var handle: SurfaceHandle?
    @Environment(AppModel.self) private var model

    final class Coordinator {
        let core: Core
        let user: UInt64
        /// The number the core gave the view, by which it stops showing the share there.
        var shown: UInt64 = 0

        init(core: Core, user: UInt64) {
            self.core = core
            self.user = user
        }
    }

    func makeCoordinator() -> Coordinator {
        Coordinator(core: model.core, user: user)
    }

    func makeUIView(context: Context) -> SampleBufferView {
        let view = SampleBufferView()
        view.backgroundColor = .black
        view.isUserInteractionEnabled = false
        view.displayLayer.videoGravity = .resizeAspect
        // The core takes over a reference to the layer.
        let layer = UInt64(UInt(bitPattern: Unmanaged.passRetained(view.displayLayer).toOpaque()))
        context.coordinator.shown = model.core.showShare(user: user, view: layer)
        handle?.view = view
        return view
    }

    func updateUIView(_ view: SampleBufferView, context: Context) {}

    static func dismantleUIView(_ view: SampleBufferView, coordinator: Coordinator) {
        coordinator.core.hideShare(user: coordinator.user, number: coordinator.shown)
    }
}

/// The share being watched in the room, at most `maxHeight` tall, with a way to fullscreen it.
struct ShareTile: View {
    let person: Person
    let share: Share
    let maxHeight: CGFloat
    @Environment(AppModel.self) private var model
    @State private var surface = SurfaceHandle()

    var body: some View {
        ZStack {
            Color.black
            // Kept under fullscreen, so the share goes back to it unchanged.
            ShareSurface(user: person.user, handle: surface)
            Loading(user: person.user)
            VStack {
                HStack(spacing: 8) {
                    Sharer(person: person, details: share.label)
                    Spacer()
                    if let warning = ShareWarning(health: model.state.health, user: person.user) {
                        WarningPill(warning: warning)
                    }
                }
                Spacer()
                HStack {
                    Spacer()
                    HStack(spacing: 16) {
                        Button("Stop Watching", systemImage: "eye.slash") {
                            model.core.command(command: .unwatch(user: person.user))
                        }
                        if AVPictureInPictureController.isPictureInPictureSupported() {
                            Button("Picture in Picture", systemImage: "pip.enter") {
                                if let view = surface.view {
                                    let size = CGSize(width: Int(share.width), height: Int(share.height))
                                    model.pictureInPicture.start(user: person.user, size: size, from: view)
                                }
                            }
                        }
                        Button("Fullscreen", systemImage: "arrow.up.left.and.arrow.down.right") {
                            model.fullscreenShare = person.user
                        }
                    }
                    .labelStyle(.iconOnly)
                    .buttonStyle(.borderless)
                    .tint(.white)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 8)
                    .background(.black.opacity(0.6), in: .capsule)
                }
            }
            .padding(8)
        }
        .aspectRatio(CGFloat(max(share.aspect, 0.5)), contentMode: .fit)
        .clipShape(.rect(cornerRadius: 16))
        .frame(maxWidth: .infinity, maxHeight: maxHeight)
    }
}

/// The share `model.fullscreenShare` over the whole screen, turning with the phone. A tap shows
/// the controls, with everyone in the room and the call's buttons along the bottom.
struct ShareFullscreen: View {
    let user: UInt64
    @Environment(AppModel.self) private var model
    @State private var controls = true

    var body: some View {
        let state = model.state
        let person = state.people.first { $0.user == user }
        ZStack {
            Color.black.ignoresSafeArea()
            if let share = person?.sharing {
                ShareSurface(user: user)
                    .aspectRatio(CGFloat(share.aspect), contentMode: .fit)
                    .ignoresSafeArea()
                Loading(user: user)
            }
            VStack {
                HStack(spacing: 8) {
                    if controls {
                        Group {
                            Button("Close", systemImage: "chevron.down") { model.fullscreenShare = nil }
                                .labelStyle(.iconOnly)
                                .buttonStyle(.bordered)
                                .buttonBorderShape(.circle)
                            Text(person?.name ?? "").foregroundStyle(.white)
                        }
                        .tint(.white)
                        .transition(.opacity)
                    }
                    Spacer()
                    if let warning = ShareWarning(health: state.health, user: user) {
                        WarningPill(warning: warning)
                    }
                }
                Spacer()
                if controls {
                    VStack(spacing: 12) {
                        RoomStrip(people: state.people.filter { $0.room == state.current?.room })
                        HStack(spacing: 16) {
                            MicButton()
                            SoundButton()
                            LeaveButton()
                        }
                        .labelStyle(.iconOnly)
                        .buttonStyle(.bordered)
                        .buttonBorderShape(.circle)
                        .controlSize(.large)
                    }
                    .transition(.opacity)
                }
            }
            .padding(12)
        }
        .contentShape(.rect)
        .onTapGesture { withAnimation { controls.toggle() } }
        .statusBarHidden(!controls)
        .persistentSystemOverlays(.hidden)
        .onAppear {
            Turning.allow(true)
            UIApplication.shared.isIdleTimerDisabled = true
        }
        .onDisappear {
            Turning.allow(false)
            UIApplication.shared.isIdleTimerDisabled = false
        }
    }
}

/// A spinner until `user`'s share has a picture to show.
private struct Loading: View {
    let user: UInt64
    @Environment(AppModel.self) private var model
    @Environment(\.theme) private var theme

    var body: some View {
        if !(model.state.current?.showing.contains(user) ?? false) {
            ProgressView().controlSize(.large).tint(theme.muted)
        }
    }
}

/// Whose share this is, over the picture.
private struct Sharer: View {
    let person: Person
    let details: String

    var body: some View {
        HStack(spacing: 8) {
            PersonPicture(person: person, size: 24)
            Text(person.name).lineLimit(1).foregroundStyle(.white)
            Text(details).lineLimit(1).foregroundStyle(.white.opacity(0.7))
        }
        .font(.footnote.weight(.medium))
        .padding(.leading, 4)
        .padding(.trailing, 10)
        .padding(.vertical, 4)
        .background(.black.opacity(0.6), in: .capsule)
    }
}

/// Everyone in the room, small.
private struct RoomStrip: View {
    let people: [Person]

    var body: some View {
        // Wraps, so a full room of 30 stays on screen.
        LazyVGrid(columns: [GridItem(.adaptive(minimum: 32, maximum: 32), spacing: 4)], spacing: 4) {
            ForEach(people, id: \.user) { person in
                PersonPicture(person: person, size: 28)
                    .padding(2)
                    .overlay {
                        if person.speaking {
                            Circle().strokeBorder(.good, lineWidth: 2)
                        }
                    }
            }
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 4)
        .background(.panel.opacity(0.8), in: .rect(cornerRadius: 20))
    }
}

/// What the pill over a watched share warns of.
private enum ShareWarning {
    case slowConnection, cantKeepUp

    /// The warning over `user`'s share, if any, the connection's first.
    init?(health: Health, user: UInt64) {
        if health.slowShares.contains(user) {
            self = .slowConnection
        } else if health.laggingShares.contains(user) {
            self = .cantKeepUp
        } else {
            return nil
        }
    }

    var label: String {
        switch self {
        case .slowConnection: "Slow connection"
        case .cantKeepUp: "Can’t keep up"
        }
    }

    var hint: String {
        switch self {
        case .slowConnection: "Frames are dropping. Ask the sharer for a lower bitrate."
        case .cantKeepUp: "This device decodes the share too slowly. Ask the sharer for a lower resolution or frame rate."
        }
    }
}

/// A warning over a share that is being watched.
private struct WarningPill: View {
    let warning: ShareWarning

    var body: some View {
        Label {
            Text(warning.label).foregroundStyle(.white)
        } icon: {
            Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.warning)
        }
        .labelStyle(.titleAndIcon)
        .font(.footnote.weight(.medium))
        .padding(.horizontal, 8)
        .padding(.vertical, 4)
        .background(.black.opacity(0.6), in: .capsule)
        .accessibilityElement(children: .combine)
        .accessibilityHint(warning.hint)
        .help(warning.hint)
    }
}

