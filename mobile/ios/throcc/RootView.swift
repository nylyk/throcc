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
                NavigationSplitView(preferredCompactColumn: $model.column) {
                    NavigationStack {
                        ServersView()
                            .navigationDestination(isPresented: $model.showsRooms) { RoomsView() }
                    }
                    .environment(\.isSidePane, sizeClass == .regular)
                } detail: {
                    RoomView()
                }
                .navigationSplitViewStyle(.balanced)
                .sheet(item: $model.sheet) { sheet in
                    content(of: sheet)
                        .modifier(NoticeBanner())
                        .modifier(Themed())
                }
            }
        }
        .modifier(NoticeBanner())
        .alert(model.alert, isPresented: $model.showsAlert) {}
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
        case .addServer: AddServerView()
        case .settings: SettingsView()
        case .newRoom: RoomEditor(room: nil)
        case .editRoom(let room): RoomEditor(room: room)
        case .person(let user): NavigationStack { PersonView(user: user) }
        case .server(let page): NavigationStack { ServerPageView(page: page) }
        }
    }
}

struct NoticeBanner: ViewModifier {
    @Environment(AppModel.self) private var model

    func body(content: Content) -> some View {
        content.overlay(alignment: .top) {
            if let notice = model.notice {
                Text(notice)
                    .font(.callout)
                    .padding(.horizontal, 16)
                    .padding(.vertical, 12)
                    .background(.raised, in: .capsule)
                    .padding(.horizontal, 16)
                    .task(id: notice) {
                        if (try? await Task.sleep(for: .seconds(4))) != nil {
                            model.notice = nil
                        }
                    }
            }
        }
    }
}

struct CloseButton: ToolbarContent {
    @Environment(AppModel.self) private var model

    var body: some ToolbarContent {
        ToolbarItem(placement: .cancellationAction) {
            Button(role: .close) { model.sheet = nil }
                .tint(.primary)
        }
    }
}
