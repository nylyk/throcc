import SwiftUI

/// The standing of the identity key the server shows for someone.
enum Trust {
    case keyChanged
    case unverified
    case verified
}

extension CoreState {
    func trust(of person: Person) -> Trust {
        if keyWarnings.contains(where: { $0.user == person.user }) {
            .keyChanged
        } else if person.user == me || person.verified {
            .verified
        } else {
            .unverified
        }
    }
}

struct TrustLabel: View {
    let trust: Trust

    var body: some View {
        switch trust {
        case .keyChanged: Text("Key changed").foregroundStyle(.bad)
        case .unverified: Text("Not verified").foregroundStyle(.muted)
        case .verified: Text("Verified").foregroundStyle(.good)
        }
    }
}
