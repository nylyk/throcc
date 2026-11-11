import SwiftUI

struct OnboardingView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.theme) private var theme
    @State private var name = ""
    @State private var picture: Data?
    @State private var refusal: String?

    var body: some View {
        let check = checkName(typed: name)
        NavigationStack {
            ThemedForm {
                Section {
                    PictureEditor(picture: $picture, name: name)
                    TextField("Name", text: $name, prompt: Text("Name").foregroundStyle(theme.muted))
                        .foregroundStyle(.text)
                        .textContentType(.nickname)
                        .autocorrectionDisabled()
                } footer: {
                    if let problem = check.problem ?? refusal {
                        Text(problem).foregroundStyle(.bad)
                    } else {
                        Text("Choose the name people see you as.").foregroundStyle(.muted)
                    }
                }
                Section {
                    Button("Continue") {
                        do {
                            try model.setProfile(name: name, picture: picture)
                        } catch {
                            refusal = error.explanation
                        }
                    }
                    .disabled(check.name == nil)
                }
            }
            .navigationTitle("Welcome to throcc")
        }
    }
}
