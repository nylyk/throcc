import AVKit
import UIKit

/// A watched share in the system's video call Picture in Picture window, which floats over
/// other apps. The core shows the share there as in any other view.
@MainActor
final class SharePictureInPicture: NSObject, AVPictureInPictureControllerDelegate {
    private let core: Core
    private var controller: AVPictureInPictureController?
    private var possible: NSKeyValueObservation?
    /// The share in the window, with the number the core gave the window's view.
    private(set) var showing: (user: UInt64, number: UInt64)?

    init(core: Core) {
        self.core = core
    }

    /// Shows `user`'s share, whose pictures are `size`, in Picture in Picture, opening from
    /// `source`.
    func start(user: UInt64, size: CGSize, from source: UIView) {
        stop()
        guard AVPictureInPictureController.isPictureInPictureSupported() else {
            return
        }
        let content = AVPictureInPictureVideoCallViewController()
        content.preferredContentSize = size
        let view = SampleBufferView()
        view.backgroundColor = .black
        view.displayLayer.videoGravity = .resizeAspect
        view.frame = content.view.bounds
        view.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        content.view.addSubview(view)
        // The core takes over a reference to the layer.
        let layer = UInt64(UInt(bitPattern: Unmanaged.passRetained(view.displayLayer).toOpaque()))
        showing = (user, core.showShare(user: user, view: layer))
        let source = AVPictureInPictureController.ContentSource(
            activeVideoCallSourceView: source,
            contentViewController: content
        )
        let controller = AVPictureInPictureController(contentSource: source)
        controller.delegate = self
        self.controller = controller
        // A new controller can start only once the system says it may.
        possible = controller.observe(\.isPictureInPicturePossible, options: [.initial, .new]) { [weak self] _, _ in
            Task { @MainActor in self?.startOnceAllowed() }
        }
    }

    private func startOnceAllowed() {
        guard possible != nil, let controller, controller.isPictureInPicturePossible else {
            return
        }
        possible = nil
        controller.startPictureInPicture()
    }

    func stop() {
        controller?.stopPictureInPicture()
        finish()
    }

    private func finish() {
        possible = nil
        if let showing {
            core.hideShare(user: showing.user, number: showing.number)
        }
        showing = nil
        controller = nil
    }

    nonisolated func pictureInPictureControllerDidStopPictureInPicture(
        _ controller: AVPictureInPictureController
    ) {
        Task { @MainActor in self.finish() }
    }

    nonisolated func pictureInPictureController(
        _ controller: AVPictureInPictureController,
        failedToStartPictureInPictureWithError error: any Error
    ) {
        Task { @MainActor in self.finish() }
    }
}
