import KithKit
import SwiftUI

/// The top of a permission card, in the order of the three voices: what the agent says (its own description, in quotes,
/// behind a rule in its colour), what Kith sees (the host's reading of the call) and the safety check. The agent's
/// words live only here: never the headline, a button or the folded line.
struct PermissionIntro: View {
    let agent: String
    var avatar: Bot?
    let intent: String?
    let facts: [CardFact]
    let check: String?
    let hostName: String
    let tint: AgentTint
    @State private var quoteOpen = false

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            if let intent, !intent.isEmpty {
                VStack(alignment: .leading, spacing: 5) {
                    HStack(spacing: 6) {
                        if let avatar { BotAvatar(bot: avatar, size: 24) }
                        Text(verbatim: "\(agent) says").kithText(.secondary).foregroundStyle(Palette.muted)
                    }
                    HStack(alignment: .top, spacing: 10) {
                        Rectangle().fill(tint.a).frame(width: 2.5)
                        // Plain text: `verbatim` never reads markdown or links from what the agent wrote.
                        Text(verbatim: "\u{201C}\(intent)\u{201D}")
                            .kithText(.body).foregroundStyle(Palette.ink)
                            .lineLimit(quoteOpen ? nil : 2)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .fixedSize(horizontal: false, vertical: true)
                    // Two lines until tapped, then all of it.
                    .contentShape(Rectangle())
                    .onTapGesture { quoteOpen.toggle() }
                    .accessibilityAddTraits(.isButton)
                    .accessibilityHint(quoteOpen ? "Shows two lines" : "Shows all of it")
                }
            }
            VStack(alignment: .leading, spacing: 6) {
                Text("Kith sees").kithText(.secondary).foregroundStyle(Palette.muted)
                ForEach(Array(facts.enumerated()), id: \.offset) { _, fact in
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        // The eye is wider than tall: a smaller size keeps it inside the 18 pt column.
                        Image(systemName: fact.symbol).font(.system(size: fact.symbol == "eye" ? 12 : 14)).foregroundStyle(Palette.muted)
                            .frame(width: 18)
                        Text(Self.line(fact, hostName: hostName)).kithText(.secondary).foregroundStyle(Palette.ink)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
                if let check, !check.isEmpty {
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        Image(systemName: "checkmark.shield").font(.system(size: 13)).foregroundStyle(Palette.muted)
                            .frame(width: 18)
                        Text(verbatim: check).kithText(.secondary).foregroundStyle(Palette.muted)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .padding(.top, 2)
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    /// The fact's text with its key nouns in bold, then the muted words after it (never markdown: built from plain pieces).
    static func line(_ fact: CardFact, hostName: String) -> AttributedString {
        var out = AttributedString()
        for piece in fact.pieces {
            var part = AttributedString(piece.text)
            if piece.bold { part.inlinePresentationIntent = .stronglyEmphasized }
            out += part
        }
        let note = fact.note(hostName: hostName)
        if !note.isEmpty {
            var part = AttributedString(" \u{00B7} \(note)")
            part.foregroundColor = Palette.muted
            out += part
        }
        return out
    }
}
