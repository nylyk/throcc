import SwiftUI

extension Color {
    init(rgb: UInt32) {
        let part = { (shift: UInt32) in Double(rgb >> shift & 0xFF) / 255 }
        self.init(red: part(16), green: part(8), blue: part(0))
    }
}

extension Color.Resolved {
    var rgb: UInt32 {
        let byte = { (part: Float) in UInt32((min(max(part, 0), 1) * 255).rounded()) }
        return byte(red) << 16 | byte(green) << 8 | byte(blue)
    }
}
