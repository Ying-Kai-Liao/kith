import KithKit

/// A command in the composer's `/` list.
struct SlashCommand: Identifiable, Equatable {
    enum Action: Hashable { case skill(String) }

    /// What follows the `/`.
    let name: String
    let icon: String
    let title: String
    let detail: String
    let warning: Bool
    let action: Action

    var id: Action { action }
}

/// The composer's `/` commands, listed while the draft is just `/partial`: a skill fills in `/<id> `, and the host turns
/// that message into "read this skill first" (`host/src/market/skills.rs` `expand_command`).
/// docs/features/marketplace.md, "Commands".
enum SlashCommands {
    /// The command name being typed after a leading `/`, before any space.
    static func query(_ draft: String) -> String? {
        guard draft.hasPrefix("/"), !draft.contains(where: \.isWhitespace) else { return nil }
        return String(draft.dropFirst())
    }

    /// The bot's skills whose name or title holds `query` (none in a group).
    static func list(for bot: Bot?, query: String, skills: [InstalledSkill]) -> [SlashCommand] {
        guard let bot, !bot.isGroup else { return [] }
        let own = skills.filter { bot.skills.contains($0.id) }.map {
            SlashCommand(name: $0.id, icon: "book.pages", title: $0.name, detail: $0.description, warning: false, action: .skill($0.id))
        }
        return own.filter {
            query.isEmpty || $0.name.localizedCaseInsensitiveContains(query) || $0.title.localizedCaseInsensitiveContains(query)
        }
    }
}
