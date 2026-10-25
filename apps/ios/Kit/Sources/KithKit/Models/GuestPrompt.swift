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

/// A guest's pending permission card as the computer's owner sees it: outside any chat the owner has, so it is
/// neither a bot in the roster nor an unread chat.
public struct GuestPrompt: Identifiable, Hashable, Sendable {
    public let entry: Entry

    public init(_ entry: Entry) { self.entry = entry }

    public var id: String { entry.id }
    /// The invitee's chat the card is in (what the push names).
    public var chatId: String { entry.botId }
    /// "<personName> · <agent>".
    public var who: String {
        let person = entry.data.personName ?? entry.data.person ?? "Someone"
        guard let agent = entry.data.agent, !agent.isEmpty else { return person }
        return "\(person) · \(agent)"
    }
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

    /// Pending cards that wait for the owner, in an invitee's chat the owner has no bot for, oldest first.
    /// `known` is the owner's own bots: a card in one of those is that bot's card, not a guest's.
    public static func pending(in entries: [String: [Entry]], known: Set<String>) -> [GuestPrompt] {
        var waiting: [Entry] = []
        for (chat, list) in entries where !known.contains(chat) {
            waiting += list.filter { $0.kind == "permission" && $0.data.status == "pending" && $0.data.approver == "owner" && !$0.data.isSecretCard }
        }
        waiting.sort { $0.createdAt != $1.createdAt ? $0.createdAt < $1.createdAt : $0.id < $1.id }
        return waiting.map { GuestPrompt($0) }
    }

    /// In the invitee's own chat: a card that waits for the owner shows this and no buttons. The host's activity line
    /// ("Waiting for Kai") carries the owner's name; without it, "the owner". On the owner's own device a card for the
    /// owner is his to answer (nil); the host still decides whose answer counts.
    public static func waitingText(for entry: Entry, activity: String?, isOwner: Bool) -> String? {
        guard !isOwner, entry.kind == "permission", entry.data.status == "pending", entry.data.approver == "owner" else { return nil }
        if let activity, activity.hasPrefix("Waiting for ") { return activity }
        return "Waiting for the owner"
    }
}
