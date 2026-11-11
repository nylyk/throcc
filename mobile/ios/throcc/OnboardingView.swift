import SwiftUI

struct OnboardingView: View {
    @Environment(AppModel.self) private var model
    @State private var name = ""
    @State private var picture: Data?
    @State private var refusal: String?

    var body: some View {
        let check = checkName(typed: name)
        NavigationStack {
            Form {
                Section {
                    PictureEditor(picture: $picture, name: name)
                    TextField("Name", text: $name)
                        .textContentType(.nickname)
                        .autocorrectionDisabled()
                } header: {
                    Text("Choose the name people see you as.")
                } footer: {
                    if let problem = check.problem ?? refusal {
                        Text(problem).foregroundStyle(.red)
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
