import SwiftUI

struct ShareScreenSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @Environment(\.theme) private var theme

    var body: some View {
        @Bindable var model = model
        let choice = model.shareChoice
        let bitrates = shareBitrates(choice: choice)
        NavigationStack {
            ThemedForm {
                Section {
                    Picker("Resolution", selection: picked(\.height)) {
                        ForEach(shareResolutions(), id: \.height) { Text($0.label).tag($0.height) }
                    }
                    .foregroundStyle(.text)
                    Picker("Frame rate", selection: picked(\.fps)) {
                        ForEach(shareFrameRates(), id: \.self) { Text("\($0) fps").tag($0) }
                    }
                    .foregroundStyle(.text)
                }
                Section {
                    LabeledContent("Bitrate") { Text(mbps(kbps: choice.kbps)).foregroundStyle(.muted) }
                        .foregroundStyle(.text)
                    if bitrates.count > 1 {
                        Slider(value: bitrateStep(bitrates), in: 0...Double(bitrates.count - 1), step: 1) {
                            Text("Bitrate")
                        } minimumValueLabel: {
                            Text(mbps(kbps: bitrates.first ?? 0)).font(.caption).foregroundStyle(.muted)
                        } maximumValueLabel: {
                            Text(mbps(kbps: bitrates.last ?? 0)).font(.caption).foregroundStyle(.muted)
                        }
                        .accessibilityValue(mbps(kbps: choice.kbps))
                    }
                }
                Section {
                    Toggle("Share device audio", isOn: $model.shareAudio)
                        .foregroundStyle(.text)
                }
            }
            .navigationTitle("Share Your Screen")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(role: .cancel) { dismiss() }
                        .tint(theme.text)
                }
                ToolbarItem(placement: .confirmationAction) {
                    ConfirmButton("Start Sharing") {
                        model.core.command(command: .startShare(choice: choice, audio: model.shareAudio))
                        dismiss()
                    }
                }
            }
        }
    }

    /// One part of the share choice, kept within what the core offers as it changes.
    private func picked<Value>(_ path: WritableKeyPath<ShareChoice, Value>) -> Binding<Value> {
        Binding {
            model.shareChoice[keyPath: path]
        } set: { value in
            var choice = model.shareChoice
            choice[keyPath: path] = value
            model.shareChoice = fitShareChoice(choice: choice)
        }
    }

    /// The bitrate as a step along `bitrates`.
    private func bitrateStep(_ bitrates: [UInt32]) -> Binding<Double> {
        Binding {
            Double(bitrates.firstIndex { $0 >= model.shareChoice.kbps } ?? bitrates.count - 1)
        } set: { step in
            let index = min(max(Int(step.rounded()), 0), bitrates.count - 1)
            model.shareChoice.kbps = bitrates[index]
        }
    }
}

struct ShareScreenButton: View {
    @Environment(AppModel.self) private var model

    var body: some View {
        if model.state.current?.sharing ?? false {
            Button("Stop Sharing", systemImage: "rectangle.on.rectangle.slash") {
                model.core.command(command: .stopShare)
            }
            .tint(model.theme.bad)
        } else {
            Button("Share Screen", systemImage: "rectangle.on.rectangle") {
                model.sheet = .shareScreen
            }
            .tint(model.theme.text)
        }
    }
}
