import KithKit
import SwiftUI

/// An agent's colour pair (AvatarPalette) as the redesign uses it: `a` is the deeper colour (lines, dots),
/// `b` the lighter one (fills, at low strength). A bot with an old `avatarColor` (no `kith:` prefix) has none and
/// gets the neutral grey, so it never turns violet.
public struct AgentTint: Equatable, Sendable {
    public let a: Color
    public let b: Color

    public init(a: Color, b: Color) { self.a = a; self.b = b }

    public init?(stored: String?) {
        guard let pick = AvatarPalette.parse(stored), let swatch = AvatarPalette.colors.first(where: { $0.id == pick.id }) else { return nil }
        // A plain pick is one colour everywhere the pair is worn.
        self.init(a: Color(hex: swatch.a), b: Color(hex: pick.plain ? swatch.a : swatch.b))
    }

    /// For an agent without a pair.
    public static let neutral = AgentTint(a: Palette.muted, b: Palette.line)

    /// The pair, or the neutral one.
    public static func resolve(_ stored: String?) -> AgentTint { AgentTint(stored: stored) ?? .neutral }
}
