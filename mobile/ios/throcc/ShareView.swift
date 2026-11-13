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
        view.displayLayer.videoGravity = .resizeAspect
        // The core takes over a reference to the layer.
        let layer = UInt64(UInt(bitPattern: Unmanaged.passRetained(view.displayLayer).toOpaque()))
        context.coordinator.shown = model.core.showShare(user: user, view: layer)
        return view
    }

    func updateUIView(_ view: SampleBufferView, context: Context) {}

    static func dismantleUIView(_ view: SampleBufferView, coordinator: Coordinator) {
        coordinator.core.hideShare(user: coordinator.user, number: coordinator.shown)
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
                // Kept under fullscreen, so the share goes back to it unchanged.
                ShareSurface(user: person.user)
                Loading(user: person.user)
                VStack {
                    HStack(spacing: 8) {
                        if model.state.health.slowShares.contains(person.user) {
                            SlowConnectionPill()
                        }
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
                    HStack {
                        Sharer(person: person)
                        Spacer()
                    }
                }
                .padding(8)
            } else {
                VStack(spacing: 8) {
                    Image(systemName: "rectangle.on.rectangle").font(.title)
                    Text("\(person.name) is sharing").lineLimit(1)
                    Text(share.label).font(.footnote).foregroundStyle(.muted)
                    Button("Watch") { model.core.watchOnly(user: person.user) }
                        .buttonStyle(.bordered)
                }
                .foregroundStyle(.text)
                .padding(16)
            }
        }
        .aspectRatio(CGFloat(max(share.aspect, 0.75)), contentMode: .fit)
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
                    if state.health.slowShares.contains(user) {
                        SlowConnectionPill()
                    }
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
        .onAppear { Turning.allow(true) }
        .onDisappear { Turning.allow(false) }
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

    var body: some View {
        HStack(spacing: 8) {
            PersonPicture(person: person, size: 24)
            Text(person.name).lineLimit(1).foregroundStyle(.white)
        }
        .font(.footnote.weight(.medium))
        .padding(.leading, 4)
        .padding(.trailing, 10)
        .padding(.vertical, 4)
        .background(.black.opacity(0.6), in: .capsule)
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

/// Says the connection can't keep up with a share that is being watched.
private struct SlowConnectionPill: View {
    var body: some View {
        Label {
            Text("Slow connection").foregroundStyle(.white)
        } icon: {
            Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.warning)
        }
        .labelStyle(.titleAndIcon)
        .font(.footnote.weight(.medium))
        .padding(.horizontal, 8)
        .padding(.vertical, 4)
        .background(.black.opacity(0.6), in: .capsule)
        .accessibilityElement(children: .combine)
        .accessibilityHint("Frames are dropping. Ask the sharer for a lower bitrate.")
        .help("Frames are dropping. Ask the sharer for a lower bitrate.")
    }
}
