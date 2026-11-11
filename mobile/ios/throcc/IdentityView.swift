import SwiftUI

struct IdentityView: View {
    private let identity = Result { try identityKey() }

    var body: some View {
        VStack(spacing: 12) {
            Text("Identity key")
                .font(.headline)
            switch identity {
            case .success(let key):
                Text(key.map { String(format: "%02x", $0) }.joined())
                    .monospaced()
                    .textSelection(.enabled)
            case .failure(let error):
                Text(error.localizedDescription)
                    .foregroundStyle(.red)
            }
        }
        .multilineTextAlignment(.center)
        .padding()
    }
}
