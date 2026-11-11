import Foundation

extension NameCheck {
    var problem: String? {
        switch self {
        case .empty, .fine: nil
        case .tooLong: "Names can be at most \(limits().nameChars) characters."
        case .showsNothing: "Names need at least one visible character."
        case .hidesText: "Names can’t contain control or invisible characters."
        }
    }

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
        case .Failed(let message): message
        case nil: localizedDescription
        }
    }
}
