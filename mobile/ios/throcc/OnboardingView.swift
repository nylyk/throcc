import SwiftUI

struct OnboardingView: View {
    @Environment(AppModel.self) private var model
    @State private var name = ""
    @State private var picture: Data?
    @State private var refusal: String?

    var body: some View {
        let check = checkName(typed: name)
        NavigationStack {
            ThemedForm {
                Section {
                    PictureEditor(picture: $picture, name: name)
                    ThemedField("Name", text: $name)
                        .textContentType(.nickname)
                        .autocorrectionDisabled()
                } footer: {
                    if let problem = nameProblem(typed: name) ?? refusal {
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
