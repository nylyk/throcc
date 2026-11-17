import SwiftUI

struct TrustLabel: View {
    let trust: KeyTrust
    var onThisDevice = false

    var body: some View {
        switch trust {
        case .keyChanged: Text("Key changed").foregroundStyle(.bad)
        case .unverified: Text("Not verified").foregroundStyle(.muted)
        case .verified: Text(onThisDevice ? "Verified on this device" : "Verified").foregroundStyle(.good)
        }
    }
}
