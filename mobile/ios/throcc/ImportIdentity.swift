import AVFoundation
import SwiftUI
import UIKit

struct ImportIdentityPage: View {
    @Environment(AppModel.self) private var model
    @State private var problem: String?

    var body: some View {
        ThemedForm {
            Section {
                Text("Bring your identity from another device that has throcc: open Settings, then Profile.")
                    .foregroundStyle(.text)
            }
            Section {
                Button("Scan QR Code") { model.onboardingPath.append(.scan) }
                OpenFileButton(problem: $problem) { file in model.onboardingPath.append(.enterCode(file)) }
                if let problem {
                    Text(problem).foregroundStyle(.bad)
                }
            }
        }
        .navigationTitle("Import Identity")
    }
}

/// Opens an exported identity from Files and hands its bytes to `open`, or says why it can't.
private struct OpenFileButton: View {
    @Binding var problem: String?
    let open: (Data) -> Void
    @State private var picking = false

    var body: some View {
        Button("Open File") { picking = true }
            .fileImporter(isPresented: $picking, allowedContentTypes: [.data]) { picked in
                do {
                    let file = try read(try picked.get())
                    guard isIdentityExport(file: file) else {
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

    private func read(_ url: URL) throws -> Data {
        let granted = url.startAccessingSecurityScopedResource()
        defer {
            if granted {
                url.stopAccessingSecurityScopedResource()
            }
        }
        let size = try url.resourceValues(forKeys: [.fileSizeKey]).fileSize ?? 0
        guard UInt64(size) <= maxExportBytes() else {
            return Data()
        }
        return try Data(contentsOf: url)
    }
}

struct ScanPage: View {
    @Environment(AppModel.self) private var model
    @State private var camera: QRCamera?
    @State private var progress: ScanProgress?
    @State private var cameraProblem: String?
    @State private var fileProblem: String?

    var body: some View {
        ThemedForm {
            if let cameraProblem {
                Section {
                    Text(cameraProblem).foregroundStyle(.text)
                    OpenFileButton(problem: $fileProblem) { file in
                        model.onboardingPath = [.importIdentity, .enterCode(file)]
                    }
                    if let fileProblem {
                        Text(fileProblem).foregroundStyle(.bad)
                    }
                }
            } else {
                Section {
                    Color.clear
                        .aspectRatio(1, contentMode: .fit)
                        .overlay {
                            if let camera {
                                CameraPreview(session: camera.session)
                            } else {
                                ProgressView()
                            }
                        }
                        .listRowInsets(EdgeInsets())
                } footer: {
                    Text("Point the camera at the QR code on your other device.").foregroundStyle(.muted)
                }
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
        .navigationTitle("Scan QR Code")
        .navigationBarTitleDisplayMode(.inline)
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
        if let file = camera.export {
            model.onboardingPath = [.importIdentity, .enterCode(file)]
        }
    }
}

/// The camera, reading the QR codes of an identity export.
final class QRCamera: NSObject, AVCaptureMetadataOutputObjectsDelegate, @unchecked Sendable {
    let session = AVCaptureSession()
    private let input: AVCaptureDeviceInput
    private let scanner = IdentityScanner()
    /// Configures and runs the session, and receives what it reads.
    private let queue = DispatchQueue(label: "cc.thro.throcc.qr-camera", qos: .userInitiated)
    /// Where `scan` tells how far it has come, touched only on `queue`.
    private var reading: AsyncStream<ScanProgress>.Continuation?

    init?(device: AVCaptureDevice?) {
        guard let device, let input = try? AVCaptureDeviceInput(device: device) else { return nil }
        self.input = input
    }

    /// The whole export, once scanned.
    var export: Data? { scanner.export() }

    /// Runs the camera and tells how far the export has come with each code it reads. The
    /// stream ends once the export is whole, and the camera stops with it.
    func scan() -> AsyncStream<ScanProgress> {
        let (progress, reading) = AsyncStream.makeStream(of: ScanProgress.self)
        reading.onTermination = { [self] _ in
            queue.async { self.session.stopRunning() }
        }
        queue.async { [self] in
            self.reading = reading
            let output = AVCaptureMetadataOutput()
            session.beginConfiguration()
            if session.canAddInput(input) {
                session.addInput(input)
            }
            if session.canAddOutput(output) {
                session.addOutput(output)
            }
            session.commitConfiguration()
            output.setMetadataObjectsDelegate(self, queue: queue)
            output.metadataObjectTypes = [.qr]
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
            if progress.done {
                reading.finish()
                self.reading = nil
            }
        }
    }
}

private struct CameraPreview: UIViewRepresentable {
    let session: AVCaptureSession

    final class PreviewView: UIView {
        override class var layerClass: AnyClass { AVCaptureVideoPreviewLayer.self }

        var preview: AVCaptureVideoPreviewLayer {
            // The layer class above makes this one.
            layer as! AVCaptureVideoPreviewLayer
        }
    }

    func makeUIView(context: Context) -> PreviewView {
        let view = PreviewView()
        view.backgroundColor = .black
        view.preview.videoGravity = .resizeAspectFill
        view.preview.session = session
        return view
    }

    func updateUIView(_ view: PreviewView, context: Context) {}
}

struct EnterCodePage: View {
    @Environment(AppModel.self) private var model
    let file: Data
    @State private var code = ""
    @State private var importing = false
    @State private var problem: String?

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
