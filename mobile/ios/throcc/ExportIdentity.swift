import SwiftUI
import UIKit

struct ExportIdentitySection: View {
    @Environment(AppModel.self) private var model
    @State private var exporting = false
    @State private var sharedCode: String?
    @State private var problem: String?

    var body: some View {
        Section {
            Text("Bring your identity, servers and the people you verified or blocked to another device. Scan the QR code with a phone, or import the file on any device. Anyone with the file and its code can sign in as you.")
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
                ShareSheet(showing: sharedCode != nil) { sent in
                    try? FileManager.default.removeItem(at: exportFile)
                    if let sharedCode, sent {
                        model.settingsPath.append(.exportCode(sharedCode))
                    }
                    sharedCode = nil
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
                sharedCode = export.code()
            } catch {
                problem = error.explanation
            }
        }
    }
}

private let exportFile = FileManager.default.temporaryDirectory.appending(path: "throcc-identity.throcc")

/// Removes an export file a share sheet left behind when the app last ended.
func removeLeftoverExport() {
    try? FileManager.default.removeItem(at: exportFile)
}

/// Hides the content while the screen is recorded or mirrored, or the app is out of focus,
/// so no recording or app switcher snapshot holds the export.
private struct HiddenWhileCaptured: ViewModifier {
    @Environment(\.isSceneCaptured) private var captured
    @Environment(\.scenePhase) private var phase

    func body(content: Content) -> some View {
        let hidden = captured || phase != .active
        content
            .opacity(hidden ? 0 : 1)
            .overlay {
                if hidden {
                    Text("Hidden while the screen is recorded or shared.")
                        .foregroundStyle(.muted)
                        .multilineTextAlignment(.center)
                        .padding(16)
                }
            }
    }
}

/// Shows the system share sheet for the export file while `showing`, and tells `done`
/// whether it was sent once it closes.
private struct ShareSheet: UIViewControllerRepresentable {
    let showing: Bool
    let done: (Bool) -> Void

    func makeUIViewController(context: Context) -> UIViewController {
        UIViewController()
    }

    func updateUIViewController(_ host: UIViewController, context: Context) {
        guard showing, host.presentedViewController == nil else { return }
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
                    Text("On the new device, choose Import from Another Device and scan this QR code. Once it’s done, choose Next.")
                        .foregroundStyle(.text)
                        .multilineTextAlignment(.center)
                    ConfirmButton("Next") {
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
        .modifier(HiddenWhileCaptured())
        .background(.bg)
        .navigationTitle("Scan on New Device")
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
            let seq = UInt32(max(0, (timeline.date.timeIntervalSince(start) / interval).rounded()))
            QRCode(frame: export.frame(seq: seq))
        }
        .accessibilityElement()
        .accessibilityLabel("QR code")
    }
}

/// A QR code in black on white, whatever the theme, so any camera reads it.
private struct QRCode: View {
    let frame: QrFrame

    var body: some View {
        Canvas { context, size in
            let width = Int(frame.width)
            let module = size.width / CGFloat(width)
            var modules = Path()
            for (index, dark) in frame.dark.enumerated() where dark {
                let x = CGFloat(index % width) * module
                let y = CGFloat(index / width) * module
                modules.addRect(CGRect(x: x, y: y, width: module, height: module))
            }
            context.fill(modules, with: .color(.black))
        }
        .aspectRatio(1, contentMode: .fit)
        .background(.white)
    }
}

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
                Text("The code unlocks the export. Do not share it with anyone else.")
                    .foregroundStyle(.muted)
                    .multilineTextAlignment(.center)
                ConfirmButton("Done") { model.settingsPath.removeLast() }
            }
            .padding(16)
            .padding(.top, 32)
            .frame(maxWidth: .infinity)
        }
        .modifier(HiddenWhileCaptured())
        .background(.bg)
        .navigationTitle("Type on New Device")
        .navigationBarTitleDisplayMode(.inline)
    }
}
