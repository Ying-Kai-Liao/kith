import KithKit
import SwiftUI

/// The avatar of a bot from outside. We never ship a provider's own logo or picture (brand guidelines, App Store 5.2.1),
/// so Grok gets Kith's ink disc with a "G" and any other or unknown provider the bubble on a faint disc. No upload.
public struct BotGuestAvatar: View {
    public enum Variant: Equatable, Sendable { case grok, other }

    let variant: Variant
    let size: CGFloat

    public init(provider: String, size: CGFloat = 26) {
        self.variant = Self.variant(provider: provider)
        self.size = size
    }

    /// Only "grok" has its own disc; empty and unknown providers read as other.
    public static func variant(provider: String) -> Variant {
        provider.lowercased() == "grok" ? .grok : .other
    }

    public var body: some View {
        Group {
            switch variant {
            case .grok:
                // The G is drawn in a 26-unit box so it centres optically and scales with the disc; it takes the ground colour.
                Circle().fill(Palette.ink)
                    .overlay {
                        Image("bot-grok-G", bundle: KithUIFonts.resources)
                            .renderingMode(.template)
                            .resizable().scaledToFit()
                            .foregroundStyle(Palette.background)
                    }
            case .other:
                Circle().fill(Palette.ink.opacity(0.06))
                    .overlay {
                        Image(systemName: "bubble.left.and.bubble.right")
                            .font(.system(size: size * 0.47, weight: .regular))
                            .foregroundStyle(Palette.muted)
                    }
            }
        }
        .frame(width: size, height: size)
        .accessibilityHidden(true)
    }
}

/// Whose message a user-kind entry is in a bot-guest chat: the bot's own, when the host says which bot sent it.
public enum BotGuestSender {
    /// The bot's name, plus "· Alice" when someone speaks for it; nil when the entry is not from a bot (or an older host
    /// sent no sender), so it renders as today.
    public static func label(_ entry: Entry, inBotGuestChat: Bool) -> String? {
        guard inBotGuestChat, entry.kind == "user", let sender = entry.data.sender, sender.botId?.isEmpty == false else { return nil }
        let name = sender.name?.isEmpty == false ? sender.name! : "Bot"
        if let speaker = sender.speaker, !speaker.isEmpty { return "\(name) · \(speaker)" }
        return name
    }
}
