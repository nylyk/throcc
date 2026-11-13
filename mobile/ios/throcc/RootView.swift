import SwiftUI

struct RootView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.scenePhase) private var scenePhase
    @Environment(\.horizontalSizeClass) private var sizeClass
    let sample: Sample?

    var body: some View {
        @Bindable var model = model
        Group {
            if model.profile == nil {
                OnboardingView()
            } else {
                let isSidePane = sizeClass == .regular
                NavigationSplitView(preferredCompactColumn: $model.column) {
                    NavigationStack {
                        ServersView()
                            .navigationDestination(isPresented: $model.showsRooms) {
                                RoomsView().environment(\.isSidePane, isSidePane)
                            }
                    }
                    .environment(\.isSidePane, isSidePane)
                } detail: {
                    RoomView()
                }
                .navigationSplitViewStyle(.balanced)
                .sheet(item: $model.sheet) {
                    model.sheetClosed()
                } content: { sheet in
                    content(of: sheet)
                        .modifier(NoticeBanner(inSheet: true))
                        .presentationBackground(model.theme.bg)
                        .modifier(Themed())
                }
            }
        }
        .modifier(NoticeBanner(inSheet: false))
        .background(.bg)
        .alert(model.alert?.title ?? "", isPresented: $model.alert.isPresent(), presenting: model.alert) { _ in
        } message: { alert in
            Text(alert.message)
        }
        .modifier(Themed())
        .task {
            if let sample {
                await model.show(sample)
            }
        }
        .onChange(of: scenePhase) {
            if scenePhase == .active {
                model.core.command(command: .reconnect)
            }
        }
    }

    @ViewBuilder
    private func content(of sheet: Sheet) -> some View {
        switch sheet {
        case .addServer(let address): AddServerView(address: address)
        case .settings: SettingsView()
        case .newRoom: RoomEditor(room: nil)
        case .editRoom(let room): RoomEditor(room: room)
        case .person(let user): NavigationStack { PersonView(user: user).toolbar { CloseButton() } }
        case .server(let page): NavigationStack { ServerPageView(page: page) }
        case .pinMismatch(let address, let code, let expected, let actual):
            PinMismatchView(address: address, code: code, expected: expected, actual: actual)
        case .identityMissing(let address, let code): IdentityMissingView(address: address, code: code)
        }
    }
}

/// Shows the model's notice over the sheet while one is up, and over the root otherwise.
struct NoticeBanner: ViewModifier {
    @Environment(AppModel.self) private var model
    let inSheet: Bool

    func body(content: Content) -> some View {
        content.overlay(alignment: .top) {
            if let notice = model.notice, inSheet == (model.sheet != nil) {
                Text(notice)
                    .font(.callout)
                    .foregroundStyle(.text)
                    .multilineTextAlignment(.center)
                    .padding(.horizontal, 16)
                    .padding(.vertical, 12)
                    .background(.raised, in: .rect(cornerRadius: 16))
                    .overlay { RoundedRectangle(cornerRadius: 16).strokeBorder(.line) }
                    .padding(.horizontal, 16)
                    .onTapGesture { model.notice = nil }
                    .task(id: notice) {
                        if (try? await Task.sleep(for: .seconds(4))) != nil {
                            model.notice = nil
                        }
                    }
            }
        }
    }
}

extension Binding {
    /// Whether the optional holds a value. Setting it to false empties the optional.
    func isPresent<Wrapped: Sendable>() -> Binding<Bool> where Value == Wrapped? {
        Binding<Bool> {
            wrappedValue != nil
        } set: { isPresent in
            if !isPresent {
                wrappedValue = nil
            }
        }
    }
}

struct CloseButton: ToolbarContent {
    @Environment(AppModel.self) private var model

    var body: some ToolbarContent {
        ToolbarItem(placement: .cancellationAction) {
            Button(role: .close) { model.sheet = nil }
                .tint(model.theme.text)
        }
    }
}
