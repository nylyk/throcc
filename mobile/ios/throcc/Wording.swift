import Foundation

extension NameCheck {
    /// The name as a server keeps it, when it takes it.
    var name: String? {
        if case .fine(let name) = self { name } else { nil }
    }
}

extension Role {
    var title: String {
        switch self {
        case .user: "User"
        case .manager: "Manager"
        case .admin: "Admin"
        }
    }
}

extension Server {
    var title: String { name ?? address }
}

extension KnownServer {
    var title: String { name ?? address }
}

extension Error {
    var explanation: String {
        switch self as? CoreError {
        case .Failed(let reason): reason
        case .PinMismatch: "The server’s key changed."
        case .IdentityMissing: "Your identity is missing from this device."
        case nil: localizedDescription
        }
    }
}
