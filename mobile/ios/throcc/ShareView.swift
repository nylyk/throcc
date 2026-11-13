import AVFoundation
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

/// `user`'s share, fitted into the view in its shape.
struct ShareSurface: UIViewRepresentable {
    let user: UInt64
    @Environment(AppModel.self) private var model

    final class Coordinator {
        let model: AppModel
        let user: UInt64

        init(model: AppModel, user: UInt64) {
            self.model = model
            self.user = user
        }
    }

    func makeCoordinator() -> Coordinator {
        Coordinator(model: model, user: user)
    }

    func makeUIView(context: Context) -> SampleBufferView {
        let view = SampleBufferView()
        view.backgroundColor = .black
        view.displayLayer.videoGravity = .resizeAspect
        model.showShare(user, in: view.displayLayer)
        return view
    }

    func updateUIView(_ view: SampleBufferView, context: Context) {}

    static func dismantleUIView(_ view: SampleBufferView, coordinator: Coordinator) {
        coordinator.model.hideShare(coordinator.user, from: view.displayLayer)
    }
}

/// A share in the room: watched in place, with a way to fullscreen it, or offered to watch.
struct ShareTile: View {
    let person: Person
    let share: Share
    @Environment(AppModel.self) private var model

    private var watching: Bool {
        model.state.current?.watching.contains(person.user) ?? false
    }

    var body: some View {
        ZStack {
            if watching {
                Color.black
                // The fullscreen view takes the share while it is open.
                if model.fullscreenShare != person.user {
                    ShareSurface(user: person.user)
                }
                VStack {
                    HStack(spacing: 8) {
                        Spacer()
                        Button("Fullscreen", systemImage: "arrow.up.left.and.arrow.down.right") {
                            model.fullscreenShare = person.user
                        }
                        Button("Stop Watching", systemImage: "xmark") {
                            model.core.command(command: .unwatch(user: person.user))
                        }
                    }
                    .labelStyle(.iconOnly)
                    .buttonStyle(.bordered)
                    .buttonBorderShape(.circle)
                    .tint(.white)
                    Spacer()
                }
                .padding(8)
            } else {
                VStack(spacing: 8) {
                    Image(systemName: "rectangle.on.rectangle").font(.title)
                    Text("\(person.name) is sharing").lineLimit(1)
                    Text(share.label).font(.footnote).foregroundStyle(.muted)
                    Button("Watch") { model.watch(person.user) }
                        .buttonStyle(.bordered)
                }
                .foregroundStyle(.text)
                .padding(16)
            }
        }
        .aspectRatio(max(share.aspect, 0.75), contentMode: .fit)
        .frame(maxWidth: .infinity)
        .background(.panel, in: .rect(cornerRadius: 16))
        .clipShape(.rect(cornerRadius: 16))
    }
}

/// The share `model.fullscreenShare` over the whole screen, turning with the phone. A tap shows
/// the controls; everyone in the room stays shown along the bottom.
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
                    .aspectRatio(share.aspect, contentMode: .fit)
                    .ignoresSafeArea()
            }
            VStack {
                if controls {
                    HStack(spacing: 8) {
                        Button("Close", systemImage: "chevron.down") { model.fullscreenShare = nil }
                            .labelStyle(.iconOnly)
                            .buttonStyle(.bordered)
                            .buttonBorderShape(.circle)
                        Text(person?.name ?? "").foregroundStyle(.white)
                        Spacer()
                    }
                    .tint(.white)
                    .transition(.opacity)
                }
                Spacer()
                RoomStrip(people: state.people.filter { $0.room == state.current?.room })
            }
            .padding(12)
        }
        .contentShape(.rect)
        .onTapGesture { withAnimation { controls.toggle() } }
        .statusBarHidden(!controls)
        .persistentSystemOverlays(.hidden)
    }
}

/// Everyone in the room, small, so nobody listens unseen while a share fills the screen.
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

extension Share {
    /// Width over height, within what a phone can show: a sender may announce any size.
    var aspect: CGFloat {
        min(max(CGFloat(width) / CGFloat(max(height, 1)), 0.25), 4)
    }
}

extension AppModel {
    /// Watches `user`'s share in place of any other: a phone watches one share at a time.
    func watch(_ user: UInt64) {
        for watched in state.current?.watching ?? [] where watched != user {
            core.command(command: .unwatch(user: watched))
        }
        core.command(command: .watch(user: user))
    }

    /// Shows `user`'s share in `layer` from now on.
    func showShare(_ user: UInt64, in layer: AVSampleBufferDisplayLayer) {
        shareLayers[user, default: []].append(layer)
        core.setShareView(user: user, view: Self.handedOver(layer))
    }

    /// Stops showing `user`'s share in `layer`, which is going away, and goes back to the one
    /// before.
    func hideShare(_ user: UInt64, from layer: AVSampleBufferDisplayLayer) {
        guard var layers = shareLayers[user], let index = layers.lastIndex(where: { $0 === layer }) else {
            return
        }
        let wasShown = index == layers.count - 1
        layers.remove(at: index)
        shareLayers[user] = layers
        guard wasShown else { return }
        core.setShareView(user: user, view: layers.last.map(Self.handedOver) ?? 0)
    }

    /// `layer` with a reference the core takes over.
    private static func handedOver(_ layer: AVSampleBufferDisplayLayer) -> UInt64 {
        UInt64(UInt(bitPattern: Unmanaged.passRetained(layer).toOpaque()))
    }
}
