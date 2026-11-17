import SwiftUI
import UIKit

struct ExportIdentitySection: View {
    @Environment(AppModel.self) private var model
    @State private var exporting = false
    @State private var shared: SharedExport?
    @State private var problem: String?

    var body: some View {
        Section {
            Text("Use your identity on another device. Anyone who gets the export and its code can sign in as you.")
                .foregroundStyle(.text)
            Button("Show QR Code") { model.settingsPath.append(.exportQR) }
                .disabled(exporting)
            Button {
                shareFile()
            } label: {
                HStack {
                    Text("Share File")
                    if exporting {
                        Spacer()
                        ProgressView()
                    }
                }
            }
            .disabled(exporting)
            .background {
                ShareSheet(shared: shared) { sent in
                    removeExportFile()
                    if let code = shared?.code, sent {
                        model.settingsPath.append(.exportCode(code))
                    }
                    shared = nil
                }
            }
            if let problem {
                Text(problem).foregroundStyle(.bad)
            }
        } header: {
            Text("Use on Another Device").foregroundStyle(.muted)
        }
    }

    private func shareFile() {
        Task {
            exporting = true
            defer { exporting = false }
            do {
                let export = try await model.exportIdentity()
                try export.file().write(to: exportFile, options: [.atomic, .completeFileProtection])
                problem = nil
                shared = SharedExport(code: export.code())
            } catch {
                problem = error.explanation
            }
        }
    }
}

/// An export being shared as a file, and the code that opens it.
private struct SharedExport {
    let code: String
}

private let exportFile = FileManager.default.temporaryDirectory.appending(path: "throcc-identity.throcc")

private func removeExportFile() {
    try? FileManager.default.removeItem(at: exportFile)
}

/// Shows the system share sheet for the export file while `shared` is set, and tells `done`
/// whether it was sent once it closes.
private struct ShareSheet: UIViewControllerRepresentable {
    let shared: SharedExport?
    let done: (Bool) -> Void

    func makeUIViewController(context: Context) -> UIViewController {
        UIViewController()
    }

    func updateUIViewController(_ host: UIViewController, context: Context) {
        guard shared != nil, host.presentedViewController == nil else { return }
        let sheet = UIActivityViewController(activityItems: [exportFile], applicationActivities: nil)
        sheet.popoverPresentationController?.sourceView = host.view
        sheet.completionWithItemsHandler = { _, completed, _, _ in done(completed) }
        host.present(sheet, animated: true)
    }
}

struct ExportQRPage: View {
    @Environment(AppModel.self) private var model
    @State private var export: IdentityExport?
    @State private var problem: String?

    var body: some View {
        ScrollView {
            VStack(spacing: 16) {
                if let export {
                    AnimatedQR(export: export)
                    Text("Scan this with throcc on the new device.")
                        .foregroundStyle(.text)
                        .multilineTextAlignment(.center)
                    ConfirmButton("Show Code") {
                        model.settingsPath.removeLast()
                        model.settingsPath.append(.exportCode(export.code()))
                    }
                } else if let problem {
                    Text(problem).foregroundStyle(.bad)
                } else {
                    ProgressView()
                }
            }
            .padding(16)
            .frame(maxWidth: .infinity)
        }
        .background(.bg)
        .navigationTitle("QR Code")
        .navigationBarTitleDisplayMode(.inline)
        .onAppear { UIApplication.shared.isIdleTimerDisabled = true }
        .onDisappear { UIApplication.shared.isIdleTimerDisabled = false }
        .task {
            do {
                export = try await model.exportIdentity()
            } catch {
                problem = error.explanation
            }
        }
    }
}

/// The export's QR codes in turn, each for `qrFrameMillis`.
private struct AnimatedQR: View {
    let export: IdentityExport
    @State private var start = Date.now

    var body: some View {
        let interval = Double(qrFrameMillis()) / 1000
        TimelineView(.periodic(from: start, by: interval)) { timeline in
            let seq = UInt32((timeline.date.timeIntervalSince(start) / interval).rounded())
            QRCode(frame: export.frame(seq: seq))
        }
        .accessibilityElement()
        .accessibilityLabel("QR code")
    }
}

/// A QR code in black on white with its quiet zone, whatever the theme, so any camera reads it.
private struct QRCode: View {
    let frame: QrFrame

    var body: some View {
        Canvas { context, size in
            let width = Int(frame.width)
            let module = size.width / CGFloat(width + 2 * quietZone)
            var modules = Path()
            for (index, dark) in frame.dark.enumerated() where dark {
                let x = CGFloat(index % width + quietZone) * module
                let y = CGFloat(index / width + quietZone) * module
                modules.addRect(CGRect(x: x, y: y, width: module, height: module))
            }
            context.fill(modules, with: .color(.black))
        }
        .aspectRatio(1, contentMode: .fit)
        .background(.white)
    }
}

/// The light modules a QR code needs around it on every side.
private let quietZone = 4

struct ExportCodePage: View {
    @Environment(AppModel.self) private var model
    let code: String

    var body: some View {
        ScrollView {
            VStack(spacing: 16) {
                Text(code)
                    .font(.system(.largeTitle, design: .monospaced))
                    .lineLimit(1)
                    .minimumScaleFactor(0.5)
                    .foregroundStyle(.text)
                Text("Type this code on the new device.")
                    .foregroundStyle(.muted)
                    .multilineTextAlignment(.center)
                ConfirmButton("Done") { model.settingsPath.removeLast() }
            }
            .padding(16)
            .padding(.top, 32)
            .frame(maxWidth: .infinity)
        }
        .background(.bg)
        .navigationTitle("Code")
        .navigationBarTitleDisplayMode(.inline)
    }
}
