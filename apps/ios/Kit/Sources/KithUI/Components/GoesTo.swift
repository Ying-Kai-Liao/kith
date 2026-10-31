import SwiftUI

/// One agent the home box can start a session with.
public struct GoesToAgent: Hashable, Sendable {
    public let key: String
    public let name: String
    /// When the agent's chat last moved; the recency order.
    public let lastAt: Int64
    /// The agent of the chat open now.
    public let selected: Bool

    public init(key: String, name: String, lastAt: Int64, selected: Bool = false) {
        self.key = key
        self.name = name
        self.lastAt = lastAt
        self.selected = selected
    }
}

/// Where the home box's message goes, before it is sent. The app picks; there is no router on the host.
/// Mirrors apps/desktop/src/shared/goes-to.ts.
public struct GoesTo: Sendable {
    public enum How: Sendable { case mention, auto, unknown, none }

    /// The agent the message goes to; nil when there is none, or an @name matched no agent.
    public let target: GoesToAgent?
    public let how: How
    /// The next two most recent other agents, to tap instead. Empty for a mention.
    public let alternatives: [GoesToAgent]
    /// Every agent, most recent first.
    public let all: [GoesToAgent]
    /// The message without the leading @name.
    public let text: String

    /// A message needs words; an unknown @name goes nowhere (it would otherwise reach the wrong agent).
    public var canSend: Bool { target != nil && !text.isEmpty }

    /// Pick order: @mention > the agent of the selected chat > the most recent chat's agent > the first agent.
    /// `agents` must be the owner's own agents only (no groups, no outside bots). Alternatives are the next two most
    /// recent distinct agents after the pick. `override` is an agent the user tapped in the pill.
    public static func route(_ agents: [GoesToAgent], query: String, override: String? = nil) -> GoesTo {
        // Stable: ties keep the roster's order, so "first agent" holds when nothing has been said yet.
        let all = agents.enumerated()
            .sorted { $0.element.lastAt != $1.element.lastAt ? $0.element.lastAt > $1.element.lastAt : $0.offset < $1.offset }
            .map(\.element)
        let trimmed = String(query.drop(while: \.isWhitespace))
        if trimmed.hasPrefix("@") {
            let rest = String(trimmed.dropFirst())
            // Longest name first, so "@Ada Lovelace hi" beats an agent named "Ada".
            let hit = all.sorted { $0.name.count > $1.name.count }.first { agent in
                let name = agent.name.trimmingCharacters(in: .whitespaces)
                guard !name.isEmpty, rest.lowercased().hasPrefix(name.lowercased()) else { return false }
                let after = rest.dropFirst(name.count)
                return after.isEmpty || after.first!.isWhitespace
            }
            guard let hit else { return GoesTo(target: nil, how: .unknown, alternatives: [], all: all, text: query) }
            let name = hit.name.trimmingCharacters(in: .whitespaces)
            let text = String(rest.dropFirst(name.count)).trimmingCharacters(in: .whitespacesAndNewlines)
            return GoesTo(target: hit, how: .mention, alternatives: [], all: all, text: text)
        }
        let text = query.trimmingCharacters(in: .whitespacesAndNewlines)
        let chosen = override.flatMap { key in all.first { $0.key == key } } ?? all.first { $0.selected } ?? all.first
        guard let chosen else { return GoesTo(target: nil, how: .none, alternatives: [], all: all, text: text) }
        return GoesTo(target: chosen, how: .auto, alternatives: Array(all.filter { $0 != chosen }.prefix(2)), all: all, text: text)
    }
}

/// "Goes to: coder · new session", in the home box while text is typed. Tapping it offers the two next most recent
/// agents and then everyone. An @name picks the agent on its own; an unknown @name shows no pill.
public struct GoesToPill: View {
    let route: GoesTo
    let onPick: (String) -> Void

    public init(route: GoesTo, onPick: @escaping (String) -> Void) {
        self.route = route
        self.onPick = onPick
    }

    private var pickable: Bool { route.how == .auto && route.all.count > 1 }

    public var body: some View {
        if let target = route.target {
            Menu {
                ForEach(route.alternatives, id: \.key) { agent in
                    Button(agent.name) { onPick(agent.key) }
                }
                Divider()
                ForEach(route.all, id: \.key) { agent in
                    Button { onPick(agent.key) } label: {
                        if agent.key == target.key { Label(agent.name, systemImage: "checkmark") } else { Text(agent.name) }
                    }
                }
            } label: {
                HStack(spacing: 4) {
                    Text("Goes to: \(target.name) · new session").kithText(.secondary).foregroundStyle(Palette.muted)
                    if pickable { Image(systemName: "chevron.down").font(.system(size: 9, weight: .bold)).foregroundStyle(Palette.muted) }
                }
                .padding(.horizontal, 10)
                .padding(.vertical, 4)
                .contentShape(Rectangle())
            }
            .disabled(!pickable)
            .frame(maxWidth: .infinity, alignment: .leading)
            .accessibilityLabel(Text("Goes to \(target.name), new session"))
        }
    }
}
