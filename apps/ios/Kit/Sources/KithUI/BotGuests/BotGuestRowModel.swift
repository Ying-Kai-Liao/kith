import KithKit
import Foundation

/// Who is looking at a bot from outside: the computer's owner, or the invited person who made it.
public enum BotViewer: Sendable {
    case owner, invitee
}

/// One bot's row as plain values: the second line and the actions by state (docs/design: bot guests UI spec 4).
public struct BotGuestRowModel: Equatable, Sendable {
    /// A conversation row opens its chat when the store has it; only a chat the store truly lacks leaves the row dead.
    public static func opens(_ conversation: BotGuestConversation, chats: [String: Bot]) -> Bool {
        conversation.chatBotId.flatMap { chats[$0] } != nil
    }

    public enum Action: Equatable, Sendable {
        case cancelInvite, newLink, deny, allow, chevron, spinner
    }

    public var title: String
    /// " · Grok", set after the name in a quieter tone.
    public var provider: String
    public var line: String
    /// The owner's own bot-waiting row: the line is in ink, with a dot.
    public var needsYou: Bool
    public var actions: [Action]

    /// `ownerName` is how the computer's owner is called to an invitee ("Kai", or "the owner of Mac mini" when the name
    /// isn't known).
    public init(_ bot: BotGuest, viewer: BotViewer, ownerName: String, now: Date = .now) {
        title = bot.name
        provider = bot.providerTitle
        needsYou = bot.state == .pending && viewer == .owner
        switch bot.state {
        case .invited:
            line = "Invite not used yet"
            actions = [.cancelInvite, .newLink]
        case .joining:
            line = "Joining…"
            actions = [.spinner]
        case .pending:
            if viewer == .owner {
                line = "Waiting for you" + (bot.waitingAge(at: now).map { " · \($0)" } ?? "")
                actions = [.deny, .allow]
            } else {
                line = "Waiting for \(ownerName) to allow it"
                actions = []
            }
        case .active:
            var text = "Asks " + (bot.agents.isEmpty ? "an agent" : bot.agents.joined(separator: ", "))
            if bot.connected {
                text += " · online now"
            } else if let seen = bot.lastSeen {
                text += " · last asked " + Self.ago(seen, now: now)
            }
            line = text
            actions = [.chevron]
        case .denied:
            line = viewer == .owner ? "You said no" : "\(Self.sentenceCase(ownerName)) said no"
            actions = []
        case .revoked:
            line = bot.revoked.map { "Removed " + Self.ago($0, now: now) } ?? "Removed"
            actions = []
        case .unknown:
            line = "Not available in this version"
            actions = []
        }
    }

    static func ago(_ date: Date, now: Date) -> String {
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .full
        formatter.locale = Locale(identifier: "en")
        return formatter.localizedString(for: date, relativeTo: now)
    }

    /// A name at the start of a sentence: "the owner of Mac mini" -> "The owner of Mac mini".
    public static func sentenceCase(_ name: String) -> String { name.prefix(1).uppercased() + name.dropFirst() }

    /// "You", the person's name, or "Automatically": who let the bot in.
    public static func allowedBy(_ bot: BotGuest, viewer: BotViewer, ownerName: String) -> String? {
        switch bot.decidedBy {
        case "kai": viewer == .owner ? "You" : sentenceCase(ownerName)
        case "owner": viewer == .invitee ? "You" : bot.ownerName ?? "Its owner"
        case "auto": "Automatically"
        default: nil
        }
    }

    /// "60 asks an hour · 2 at once".
    public static func limits(_ bot: BotGuest) -> String? {
        guard let limits = bot.limits else { return nil }
        return "\(limits.ratePerHour) asks an hour · \(limits.concurrent) at once"
    }
}

public extension BotStore {
    /// The agents a bot invite from this account may offer: an owner's agents set up on this computer, an invited
    /// person's own grants. `title` is what the toggle is called.
    var botAgentChoices: [(agent: String, title: String)] {
        if let granted = hello?.grantedAgents { return granted.map { ($0.agent, $0.title) } }
        var byAgent: [String: String] = [:]
        for bot in bots.values.sorted(by: { $0.name < $1.name }) {
            guard let agent = bot.hqAgent, !agent.isEmpty, byAgent[agent] == nil else { continue }
            byAgent[agent] = bot.name
        }
        return byAgent.sorted { $0.key < $1.key }.map { ($0.key, $0.value) }
    }

    /// How the computer's owner is called to an invited person: no name travels with the invite, so the computer's.
    var ownerCalled: String { "the owner of \(hostName)" }
}
