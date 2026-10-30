import Foundation

// What the account is on a computer, and the owner's view of a guest's permission prompt (host `agent/bot/updates.rs`).
// The data and the answer rules live here; the screens only lay them out.

/// What this account is on one computer, for labels: "Owner", or for an invited person "Member" when any agent they
/// hold is a member's, else "Guest".
public enum AccessLevel: String, Sendable {
    case owner, member, guest

    public var title: String {
        switch self {
        case .owner: "Owner"
        case .member: "Member"
        case .guest: "Guest"
        }
    }

    /// `granted` is the member hello's list; nil (not loaded yet, or an older host) is the least: guest.
    public static func of(role: ComputerRole, granted: [GrantedAgent]?) -> AccessLevel {
        guard role == .member else { return .owner }
        return granted?.contains { $0.role == .member } == true ? .member : .guest
    }

    /// The role on one agent; nil for an agent that isn't granted.
    public static func agentRole(_ agent: String?, granted: [GrantedAgent]?) -> InviteRole? {
        guard let agent else { return nil }
        return granted?.first { $0.agent == agent }?.role
    }
}

/// A pending card outside any chat this account has, waiting for its answer: a guest's (for the owner), or one in an
/// agent's card lane (`cards:<agent>`, an agent with no chat on the computer) for whoever the host names. Neither a bot
/// in the roster nor an unread chat.
public struct GuestPrompt: Identifiable, Hashable, Sendable {
    public let entry: Entry

    public init(_ entry: Entry) { self.entry = entry }

    public var id: String { entry.id }
    /// The invitee's chat or the card lane the card is in (what the push names).
    public var chatId: String { entry.botId }
    /// "<personName> · <agent>", or the agent alone for the owner's own run.
    public var who: String {
        let person = [entry.data.personName, entry.data.person].compactMap { $0 }.first { !$0.isEmpty }
        guard let agent = entry.data.agent, !agent.isEmpty else { return person ?? "Someone" }
        return person.map { "\($0) · \(agent)" } ?? agent
    }

    /// Whether a chat id is an agent's card lane: never a bot, so a push naming it opens the waiting cards.
    public static func isCardLane(_ chatId: String) -> Bool { chatId.hasPrefix("cards:") }
    /// What the agent wants to do.
    public var request: String { entry.data.title ?? "" }
    /// Only what the card offered, allow first.
    public var options: [PermissionOption] {
        let rank = ["allow_once": 0, "allow_always": 1, "reject_once": 2, "reject_always": 3]
        return (entry.data.options ?? []).sorted { (rank[$0.kind] ?? 9) < (rank[$1.kind] ?? 9) }
    }

    public static func label(_ option: PermissionOption) -> String {
        switch option.kind {
        case "allow_once": "Allow once"
        case "allow_always": "Always allow"
        case "reject_once": "Deny"
        case "reject_always": "Never"
        default: option.name
        }
    }

    /// An edge card is labelled in the host's words, since its two allow_always ("Always for Sam", "Turn on <job>")
    /// grant different things. Any other card goes by kind.
    public static func label(_ option: PermissionOption, edge: PermissionEdge?) -> String {
        edge != nil && !option.name.isEmpty ? option.name : label(option)
    }

    /// Pending cards outside this account's bots that wait for it, oldest first: on a computer it owns those for the
    /// owner, elsewhere those the host says it may answer (`canAnswer`). `known` is the account's own bots: a card in one
    /// of those is that bot's card. One whose approver changed drops out with the host's update.
    public static func pending(in entries: [String: [Entry]], known: Set<String>, isOwner: Bool) -> [GuestPrompt] {
        var waiting: [Entry] = []
        for (chat, list) in entries where !known.contains(chat) {
            waiting += list.filter {
                $0.kind == "permission" && $0.data.status == "pending" && (isOwner ? $0.data.approver == "owner" : $0.data.canAnswer == true)
            }
        }
        waiting.sort { $0.createdAt != $1.createdAt ? $0.createdAt < $1.createdAt : $0.id < $1.id }
        return waiting.map { GuestPrompt($0) }
    }

    /// A card this account can't answer shows this and no buttons. The host says which those are (`canAnswer`, the
    /// rule its guard applies); an older host doesn't, and then a card for the owner is the owner's own on the owner's
    /// device. The host's activity line ("Waiting for Kai") carries the owner's name; without it, "the owner".
    public static func waitingText(for entry: Entry, activity: String?, isOwner: Bool) -> String? {
        guard entry.kind == "permission", entry.data.status == "pending" else { return nil }
        if let can = entry.data.canAnswer {
            guard !can else { return nil }
        } else {
            guard !isOwner, entry.data.approver == "owner" else { return nil }
        }
        if let activity, activity.hasPrefix("Waiting for ") { return activity }
        return "Waiting for the owner"
    }
}
