import AVFoundation
import SwiftUI
import UIKit

struct ImportIdentityPage: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        ThemedForm {
            Section {
                Text("Bring your identity from another device that has throcc: open Settings, then Profile.")
                    .foregroundStyle(.text)
            }
            Section {
                Button("Scan QR Code") { model.onboardingPath.append(.scan) }
                OpenFileButton { file in model.onboardingPath.append(.enterCode(file, scanned: nil)) }
            }
        }
        .navigationTitle("Import Identity")
    }
}

/// Opens an exported identity from Files and hands its bytes to `open`, or says why it can't.
private struct OpenFileButton: View {
    let open: (Data) -> Void
    @State private var picking = false
    @State private var problem: String?

    var body: some View {
        Button("Open File") { picking = true }
            .fileImporter(isPresented: $picking, allowedContentTypes: [.data]) { picked in
                Task { await read(picked) }
            }
        if let problem {
            Text(problem).foregroundStyle(.bad)
        }
    }

    private func read(_ picked: Result<URL, Error>) async {
        do {
            let url = try picked.get()
            let file = try await Task.detached(priority: .userInitiated) { try readExport(url) }.value
            guard UInt64(file.count) <= maxExportBytes(), isIdentityExport(file: file) else {
                problem = "That isn’t an identity exported from throcc."
                return
            }
            problem = nil
            open(file)
        } catch {
            problem = "Couldn’t read that file."
        }
    }
}

/// The file at `url`, cut off past `maxExportBytes`.
private func readExport(_ url: URL) throws -> Data {
    let granted = url.startAccessingSecurityScopedResource()
    defer {
        if granted {
            url.stopAccessingSecurityScopedResource()
        }
    }
    let handle = try FileHandle(forReadingFrom: url)
    defer { try? handle.close() }
    return try handle.read(upToCount: Int(maxExportBytes()) + 1) ?? Data()
}

struct ScanPage: View {
    @Environment(AppModel.self) private var model
    @State private var camera: QRCamera?
    @State private var progress: ScanProgress?
    @State private var cameraProblem: String?

    var body: some View {
        let received = progress?.done == true
        ThemedForm {
            if let cameraProblem {
                Section {
                    Text(cameraProblem).foregroundStyle(.text)
                    OpenFileButton { file in
                        model.onboardingPath = [.importIdentity, .enterCode(file, scanned: nil)]
                    }
                }
            } else {
                Section {
                    Color.clear
                        .aspectRatio(1, contentMode: .fit)
                        .overlay {
                            if let camera {
                                CameraPreview(camera: camera)
                            } else {
                                ProgressView()
                            }
                        }
                        .listRowInsets(EdgeInsets())
                } footer: {
                    if received {
                        Text("Choose Next on your other device, then scan the code.").foregroundStyle(.muted)
                    } else {
                        Text("Point the camera at the QR code on your other device.").foregroundStyle(.muted)
                    }
                }
                if received {
                    Section {
                        Label("Export received", systemImage: "checkmark.circle.fill")
                            .font(.title3.weight(.semibold))
                            .foregroundStyle(.good)
                    }
                } else {
                    Section {
                        ProgressView(value: progress?.fraction ?? 0)
                            .animation(.easeOut(duration: 0.3), value: progress?.fraction)
                    } footer: {
                        if let size = progress?.size {
                            Text(size).monospacedDigit().foregroundStyle(.muted)
                        }
                    }
                }
            }
        }
        .navigationTitle("Scan QR Code")
        .navigationBarTitleDisplayMode(.inline)
        .sensoryFeedback(.success, trigger: received)
        .onAppear { UIApplication.shared.isIdleTimerDisabled = true }
        .onDisappear { UIApplication.shared.isIdleTimerDisabled = false }
        .task { await scan() }
    }

    private func scan() async {
        guard await AVCaptureDevice.requestAccess(for: .video) else {
            cameraProblem = "throcc needs the camera to scan the QR code. Allow it in the settings, or open the exported file instead."
            return
        }
        guard let camera = QRCamera(device: .default(for: .video)) else {
            cameraProblem = "This device has no camera. Open the exported file instead."
            return
        }
        self.camera = camera
        for await progress in camera.scan() {
            self.progress = progress
        }
        if let file = camera.export, let code = progress?.code {
            model.onboardingPath = [.importIdentity, .enterCode(file, scanned: code)]
        }
    }
}

/// The camera, reading the QR codes of an identity export.
final class QRCamera: NSObject, AVCaptureMetadataOutputObjectsDelegate, @unchecked Sendable {
    let session = AVCaptureSession()
    let device: AVCaptureDevice
    private let scanner = IdentityScanner()
    /// Runs the session, and receives what it reads.
    private let queue = DispatchQueue(label: "cc.thro.throcc.qr-camera", qos: .userInitiated)
    /// Where `scan` tells how far it has come, touched only on `queue`.
    private var reading: AsyncStream<ScanProgress>.Continuation?

    init?(device: AVCaptureDevice?) {
        guard let device, let input = try? AVCaptureDeviceInput(device: device) else { return nil }
        self.device = device
        super.init()
        let output = AVCaptureMetadataOutput()
        session.beginConfiguration()
        if session.isMultitaskingCameraAccessSupported {
            session.isMultitaskingCameraAccessEnabled = true
        }
        if session.canAddInput(input) {
            session.addInput(input)
        }
        if session.canAddOutput(output) {
            session.addOutput(output)
        }
        session.commitConfiguration()
        output.setMetadataObjectsDelegate(self, queue: queue)
        output.metadataObjectTypes = [.qr]
    }

    /// The whole export, once scanned.
    var export: Data? { scanner.export() }

    /// Runs the camera and tells how far the export has come with each code it reads. The
    /// stream ends once the export is whole and its code scanned, and the camera stops with it.
    func scan() -> AsyncStream<ScanProgress> {
        let (progress, reading) = AsyncStream.makeStream(of: ScanProgress.self)
        reading.onTermination = { [self] _ in
            queue.async { self.session.stopRunning() }
        }
        queue.async { [self] in
            self.reading = reading
            session.startRunning()
        }
        return progress
    }

    func metadataOutput(
        _ output: AVCaptureMetadataOutput,
        didOutput metadataObjects: [AVMetadataObject],
        from connection: AVCaptureConnection
    ) {
        for case let code as AVMetadataMachineReadableCodeObject in metadataObjects {
            guard let text = code.stringValue, let reading else { continue }
            let progress = scanner.add(text: text)
            reading.yield(progress)
            if progress.done, progress.code != nil {
                reading.finish()
                self.reading = nil
            }
        }
    }
}

private struct CameraPreview: UIViewRepresentable {
    let camera: QRCamera

    final class PreviewView: UIView {
        override class var layerClass: AnyClass { AVCaptureVideoPreviewLayer.self }
        private var rotation: AVCaptureDevice.RotationCoordinator?
        private var turning: NSKeyValueObservation?

        var preview: AVCaptureVideoPreviewLayer {
            // The layer class above makes this one.
            layer as! AVCaptureVideoPreviewLayer
        }

        /// Shows what `camera` sees, kept upright as the device turns.
        func show(_ camera: QRCamera) {
            preview.session = camera.session
            let rotation = AVCaptureDevice.RotationCoordinator(device: camera.device, previewLayer: preview)
            turning = rotation.observe(\.videoRotationAngleForHorizonLevelPreview, options: .initial) { [weak self] rotation, _ in
                let angle = rotation.videoRotationAngleForHorizonLevelPreview
                Task { @MainActor in self?.turn(to: angle) }
            }
            self.rotation = rotation
        }

        private func turn(to angle: CGFloat) {
            guard let connection = preview.connection, connection.isVideoRotationAngleSupported(angle) else { return }
            connection.videoRotationAngle = angle
        }
    }

    func makeUIView(context: Context) -> PreviewView {
        let view = PreviewView()
        view.backgroundColor = .black
        view.preview.videoGravity = .resizeAspectFill
        view.show(camera)
        return view
    }

    func updateUIView(_ view: PreviewView, context: Context) {}
}

struct EnterCodePage: View {
    @Environment(AppModel.self) private var model
    let file: Data
    let scanned: String?
    @State private var code: String
    @State private var importing = false
    @State private var problem: String?

    init(file: Data, scanned: String?) {
        self.file = file
        self.scanned = scanned
        _code = State(initialValue: scanned ?? "")
    }

    var body: some View {
        ThemedForm {
            Section {
                ThemedField("Code", text: $code)
                    .font(.body.monospaced())
                    .textInputAutocapitalization(.characters)
                    .autocorrectionDisabled()
                    .onSubmit(importIdentity)
            } footer: {
                Text("Type the code the other device shows.").foregroundStyle(.muted)
            }
            if let problem {
                Text(problem).foregroundStyle(.bad)
            }
            Section {
                Button(action: importIdentity) {
                    HStack {
                        Text(importing ? "Importing…" : "Import")
                        if importing {
                            Spacer()
                            ProgressView()
                        }
                    }
                }
                .disabled(!canImport)
            }
        }
        .navigationTitle("Import Identity")
        .navigationBarBackButtonHidden(importing)
        .onAppear {
            if scanned != nil, problem == nil {
                importIdentity()
            }
        }
    }

    private var canImport: Bool {
        !importing && !code.trimmingCharacters(in: .whitespaces).isEmpty
    }

    private func importIdentity() {
        guard canImport else { return }
        Task {
            importing = true
            defer { importing = false }
            do {
                try await model.importIdentity(file: file, code: code)
            } catch {
                problem = error.explanation
            }
        }
    }
}
