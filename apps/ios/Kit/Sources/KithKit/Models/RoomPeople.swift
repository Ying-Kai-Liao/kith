import Foundation

// People in a group room (designer SPEC "Rooms with people"; the same rules as the desktop's shared/room-people.ts):
// who can open the room, what the owner has to fix, the add picker's groups, the hidden-message marker and the invite
// sheet's room switches. Pure; no host calls here.

/// A room's agent: the Kith agent behind one of its member bots (`id` is the agent name roles are keyed by).
public struct RoomAgent: Hashable, Sendable, Identifiable {
    public var id: String
    public var name: String

    public init(id: String, name: String) {
        self.id = id
        self.name = name
    }
}

public typealias RoomStay = HostContract.RoomPerson

/// Why a group takes no people: an outside bot is in it, or no agent is.
public enum RoomBlock: Hashable, Sendable {
    case outsideBot(String)
    case noAgents

    public func text(room: String) -> String {
        switch self {
        case let .outsideBot(name): "People can't join a room with an outside bot. Remove \(name) from \(room) first."
        case .noAgents: "Add an agent to \(room) first."
        }
    }
}

public struct NeedsKaiRow: Hashable, Sendable, Identifiable {
    public enum Kind: Hashable, Sendable { case missing, revoked }
    public var personId: String
    public var name: String
    public var kind: Kind
    public var missing: [RoomAgent]
    public var id: String { personId }
}

public struct PickerRow: Hashable, Sendable, Identifiable {
    public var person: HostPerson
    public var missing: [RoomAgent]
    /// Has accepted the invite; false means the add waits for it.
    public var accepted: Bool
    public var id: String { person.personId }
}

public struct InvitedRow: Hashable, Sendable, Identifiable {
    public enum State: Hashable, Sendable { case waiting, accepted, ranOut }
    public var invite: HostInvite
    public var name: String
    public var state: State
    public var missing: [RoomAgent]
    public var id: String { invite.inviteId }
}

public enum RoomPeopleLogic {
    /// Splits a room's member ids into its Kith agents and whatever is outside (an id with no agent behind it, a bot guest).
    public static func parts(members: [String], find: (String) -> Bot?, outsideName: (String) -> String) -> (agents: [RoomAgent], outside: [String]) {
        var agents: [RoomAgent] = []
        var outside: [String] = []
        for id in members {
            if let bot = find(id), let agent = bot.hqAgent, !agent.isEmpty, bot.botGuest == nil {
                if !agents.contains(where: { $0.id == agent }) { agents.append(RoomAgent(id: agent, name: bot.name)) }
            } else {
                outside.append(find(id)?.name ?? outsideName(id))
            }
        }
        return (agents, outside)
    }

    public static func block(agents: [RoomAgent], outside: [String]) -> RoomBlock? {
        if let first = outside.first { return .outsideBot(first) }
        return agents.isEmpty ? .noAgents : nil
    }

    public static func holds(_ person: HostPerson, _ agent: String) -> Bool { person.agents[agent] != nil || person.everyoneAgents[agent] != nil }

    public static func missing(_ person: HostPerson, agents: [RoomAgent]) -> [RoomAgent] { agents.filter { !holds(person, $0.id) } }

    public static func openStay(_ stays: [RoomStay], person: String) -> RoomStay? {
        stays.first { $0.person == person && $0.leftGen == nil }
    }

    public static func joined(_ names: [String]) -> String {
        switch names.count {
        case 0: ""
        case 1: names[0]
        case 2: "\(names[0]) and \(names[1])"
        default: names.dropLast().joined(separator: ", ") + " and " + names[names.count - 1]
        }
    }

    /// "Doesn't have coder any more", "coder or video", else "3 of the agents here".
    public static func lackLine(_ missing: [RoomAgent]) -> String {
        missing.count >= 3 ? "Doesn't have \(missing.count) of the agents here" : "Doesn't have \(missing.map(\.name).joined(separator: " or ")) any more"
    }

    /// Open stays that can't open the room right now: the decisions on top of the People list.
    public static func needsKai(stays: [RoomStay], people: [HostPerson], agents: [RoomAgent]) -> [NeedsKaiRow] {
        stays.compactMap { stay in
            guard stay.leftGen == nil, let person = people.first(where: { $0.personId == stay.person }) else { return nil }
            if person.revoked { return NeedsKaiRow(personId: person.personId, name: person.name, kind: .revoked, missing: []) }
            let lacking = missing(person, agents: agents)
            return lacking.isEmpty ? nil : NeedsKaiRow(personId: person.personId, name: person.name, kind: .missing, missing: lacking)
        }
    }

    public static func inTheRoom(stays: [RoomStay], people: [HostPerson], agents: [RoomAgent]) -> [HostPerson] {
        let stuck = Set(needsKai(stays: stays, people: people, agents: agents).map(\.personId))
        return stays.compactMap { stay in
            guard stay.leftGen == nil, !stuck.contains(stay.person) else { return nil }
            return people.first { $0.personId == stay.person }
        }
    }

    /// People whose every stay here is closed: collapsed under "Earlier".
    public static func earlier(stays: [RoomStay], people: [HostPerson]) -> [HostPerson] {
        var seen = Set<String>()
        var out: [HostPerson] = []
        for stay in stays where seen.insert(stay.person).inserted {
            if openStay(stays, person: stay.person) != nil { continue }
            if let person = people.first(where: { $0.personId == stay.person }), !person.revoked { out.append(person) }
        }
        return out
    }

    /// Invites whose part for this room isn't done: one row per person, their newest invite.
    public static func invitedRows(invites: [HostInvite], people: [HostPerson], room: String, agents: [RoomAgent], now: Date = .now) -> [InvitedRow] {
        var rows: [String: InvitedRow] = [:]
        var order: [String] = []
        for invite in invites.sorted(by: { $0.createdAt > $1.createdAt }) {
            guard let part = invite.rooms?.first(where: { $0.room == room }), !part.done, rows[invite.personId] == nil else { continue }
            let person = people.first { $0.personId == invite.personId }
            let state: InvitedRow.State = invite.revokedAt != nil || (invite.usedAt == nil && invite.expiry <= now) ? .ranOut : invite.usedAt != nil ? .accepted : .waiting
            rows[invite.personId] = InvitedRow(invite: invite, name: person?.name ?? invite.personId, state: state, missing: person.map { missing($0, agents: agents) } ?? [])
            order.append(invite.personId)
        }
        return order.compactMap { rows[$0] }
    }

    /// The add picker: invited people not revoked and not in the room now, split by whether they hold every room agent.
    public static func pickerGroups(people: [HostPerson], stays: [RoomStay], agents: [RoomAgent]) -> (canJoin: [PickerRow], needsAccess: [PickerRow]) {
        var canJoin: [PickerRow] = []
        var needsAccess: [PickerRow] = []
        for person in people where !person.revoked && openStay(stays, person: person.personId) == nil {
            let lacking = missing(person, agents: agents)
            let row = PickerRow(person: person, missing: lacking, accepted: person.joined)
            if lacking.isEmpty { canJoin.append(row) } else { needsAccess.append(row) }
        }
        return (canJoin, needsAccess)
    }

    /// Roles to save to give a person agents: what they hold already, plus these.
    public static func grantRoles(_ person: HostPerson, given: [String: InviteRole]) -> [String: InviteRole] {
        person.agents.merging(given) { _, new in new }
    }

    /// "coder won't answer here. Ben doesn't have coder.": the line above Kai's box, nil when nothing is wrong.
    public static func wontAnswerLine(_ rows: [NeedsKaiRow], agents: [RoomAgent]) -> String? {
        guard !rows.isEmpty else { return nil }
        let revoked = rows.filter { $0.kind == .revoked }
        // A revoked person closes the room for every agent.
        if !revoked.isEmpty { return "No agent answers here while \(joined(revoked.map(\.name)))'s access is revoked." }
        let silent = agents.filter { agent in rows.contains { $0.missing.contains(agent) } }.map(\.name)
        let names = rows.map(\.name)
        let reason = silent.count == 1 && names.count == 1 ? "\(names[0]) doesn't have \(silent[0])." : "\(joined(names)) \(names.count == 1 ? "is" : "are") missing access."
        return "\(joined(silent)) won't answer here. \(reason)"
    }

    /// The invite sheet's Rooms group: a room is on only while every agent it needs is switched on above.
    public static func roomNeeds(_ roomAgents: [RoomAgent], roles: [String: InviteRole]) -> [RoomAgent] { roomAgents.filter { roles[$0.id] == nil } }

    public static func followAgents(ticked: [String], rooms: [(id: String, agents: [RoomAgent])], roles: [String: InviteRole]) -> [String] {
        ticked.filter { id in rooms.first(where: { $0.id == id }).map { roomNeeds($0.agents, roles: roles).isEmpty } ?? false }
    }

    /// "Mia joins Studio when Mia accepts."
    public static func inviteJoinsLine(name: String, rooms: [String]) -> String? {
        rooms.isEmpty ? nil : "\(name) joins \(joined(rooms)) when \(name) accepts."
    }

    /// Settings: rooms a person is in whose agents they would no longer all hold after the change.
    public static func closingRooms(person: HostPerson, newRoles: [String: InviteRole], inRooms: [(room: String, name: String, agents: [RoomAgent])]) -> [(room: String, name: String, lost: [RoomAgent])] {
        inRooms.compactMap { r in
            let lost = r.agents.filter { newRoles[$0.id] == nil && person.everyoneAgents[$0.id] == nil }
            return lost.isEmpty ? nil : (r.room, r.name, lost)
        }
    }

    // MARK: the chat

    /// The names a message is marked "From before <names> joined" for: only below that person's join line.
    public static func hiddenFromNames(_ entry: Entry, stays: [RoomStay], nameOf: (String) -> String) -> [String] {
        (entry.data.hiddenFrom ?? []).filter { id in stays.contains { $0.person == id && entry.seq > $0.joinedGen } }.map(nameOf)
    }

    public static func hiddenMarkerText(_ names: [String]) -> String {
        names.count > 2 ? "Hidden from \(names.count) people" : "From before \(joined(names)) joined"
    }

    public static func hiddenMarkerHelp(_ names: [String], agent: String?) -> String {
        agent.map { "\($0) started this answer before \(joined(names)) joined, so it was written with your access. Only you see it." } ?? "Only you see this."
    }

    public static func isPersonNotice(_ entry: Entry) -> Bool {
        entry.kind == "notice" && (entry.data.notice == "personJoined" || entry.data.notice == "personLeft")
    }

    /// The join divider's words, and the muted second line on the room's first join.
    public static func joinLines(_ entry: Entry, firstJoin: Bool) -> [String] {
        let name = entry.data.person?.name ?? "Someone"
        if entry.data.notice == "personLeft" { return ["\(name) left"] }
        return firstJoin ? ["\(name) joined · sees from here", "Agents now answer only when @'d."] : ["\(name) joined · sees from here"]
    }

    public static func isFirstJoin(_ entry: Entry, in entries: [Entry]) -> Bool {
        !entries.contains { $0.data.notice == "personJoined" && $0.seq < entry.seq }
    }

    /// The waiting notice PR C writes ("<agent> is waiting for the owner to answer a card."): the agent's name, else nil.
    public static func waitingAgent(_ entry: Entry) -> String? {
        guard entry.kind == "notice", let text = entry.data.text else { return nil }
        let suffix = " is waiting for the owner to answer a card."
        return text.hasSuffix(suffix) ? String(text.dropLast(suffix.count)) : nil
    }

    /// The viewer's own message a "didn't answer" line is about: the newest of theirs above it (the host names none).
    public static func unansweredTrigger(_ entry: Entry, in entries: [Entry]) -> Entry? {
        guard let at = entries.firstIndex(where: { $0.id == entry.id }) else { return nil }
        return entries[..<at].last { $0.kind == "user" && $0.data.sender == nil && ($0.data.hiddenFrom ?? []).isEmpty && $0.data.status != "cancelled" }
    }
}

/// An invited person's side (SPEC §8-§10): their rooms read as groups in the roster, with the two opening lines, an
/// "Asked <agent>" line and one neutral closed state.
public enum MemberRoomLogic {
    /// A group-shaped bot for a room in `myRooms` (the app has no member bots for it; its agents are `MyRoom.agents`).
    public static func bot(for room: HostContract.MyRoom, lastAt: Int64? = nil, lastMessage: String? = nil) -> Bot? {
        struct Seed: Encodable { var id: String; var kind = "group"; var name: String; var members: [String] = []; var lastAt: Int64?; var lastMessage: String? }
        guard let data = try? JSONEncoder().encode(Seed(id: room.id, name: room.name, lastAt: lastAt, lastMessage: lastMessage)) else { return nil }
        return try? JSONDecoder().decode(Bot.self, from: data)
    }

    /// The rooms that went away between two lists: closed for the person, their cache dropped.
    public static func closed(before: Set<String>, after: [HostContract.MyRoom]) -> Set<String> {
        before.subtracting(after.map(\.id))
    }

    /// The rooms a launch may have cached: the ones kept with their stay, and a group bot with no agent or member behind it
    /// (a cache from before rooms were kept). The first fresh list closes whichever it does not name.
    public static func cachedRoomIds(restored: [String], bots: [Bot]) -> Set<String> {
        Set(restored).union(bots.filter { $0.isGroup && $0.hqAgent == nil && $0.members.isEmpty }.map(\.id))
    }

    /// The host refused the call because the stay is over (403/404), as opposed to a link that dropped.
    public static func isClosedRefusal(_ error: Error) -> Bool {
        guard case let HostError.http(status, _) = error else { return false }
        return status == 403 || status == 404
    }

    /// What a person calls the owner. The host does not send the owner's name yet (follow-up: `myRooms` carries it).
    public static let ownerLabel = "the owner"

    private static var ownerCapitalized: String { ownerLabel.prefix(1).uppercased() + ownerLabel.dropFirst() }

    public static func startLines(room: String) -> [String] { ["You see \(room) from when \(ownerLabel) added you.", "\(ownerCapitalized) sees everything said here."] }

    /// A cached room whose stay differs from the list's (or that no stay was kept for): its messages belong to an earlier stay.
    public static func stayChanged(known: HostContract.MyRoom?, fresh: HostContract.MyRoom) -> Bool { known?.stay != fresh.stay }

    public static func closedLine(room: String) -> String { "You're no longer in \(room)." }

    /// "Asked planner" under the person's newest message that @'d an agent, until an agent reply lands after it.
    public static func askedLine(entries: [Entry], agents: [String]) -> String? {
        for e in entries.reversed() {
            if e.kind == "agent" { return nil }
            guard e.kind == "user" else { continue }
            let text = (e.data.text ?? "").lowercased()
            let asked = agents.filter { agent in
                let tag = "@" + agent.lowercased()
                var from = text.startIndex
                while let r = text.range(of: tag, range: from..<text.endIndex) {
                    let before = r.lowerBound == text.startIndex || text[text.index(before: r.lowerBound)].isWhitespace
                    let after = r.upperBound == text.endIndex || !(text[r.upperBound].isLetter || text[r.upperBound].isNumber || text[r.upperBound] == "-")
                    if before && after { return true }
                    from = r.upperBound
                }
                return false
            }
            return asked.isEmpty ? nil : "Asked \(RoomPeopleLogic.joined(asked))"
        }
        return nil
    }

    /// Room info for a person: one muted line, never a people list.
    public static func ownerLine(room: String) -> String { "\(room) is \(ownerLabel)'s room, on \(ownerLabel)'s computer. \(ownerCapitalized) sees everything said here." }
}

public extension RoomPeopleLogic {
    /// The invite sheet's room options (Kai's side): only groups that take people, i.e. only Kith agents in them.
    static func inviteRooms(groups: [Bot], find: (String) -> Bot?) -> [(id: String, name: String, agents: [RoomAgent])] {
        groups.filter(\.isGroup).compactMap { g in
            let p = parts(members: g.members, find: find, outsideName: { _ in "Bot" })
            return p.outside.isEmpty && !p.agents.isEmpty ? (g.id, g.name, p.agents) : nil
        }
    }

    /// A new invite from a room's Add: its agents on as guest and the room ticked.
    static func roomInviteStart(agents: [RoomAgent], room: String) -> (roles: [String: InviteRole], rooms: [String]) {
        (Dictionary(uniqueKeysWithValues: agents.map { ($0.id, InviteRole.guest) }), [room])
    }

    /// The warning under an agent switch in the edit sheet, one per room that would close for this person.
    static func closingLines(person: String, closing: [(room: String, name: String, lost: [RoomAgent])]) -> [String] {
        closing.map { c in
            let who = joined(c.lost.map(\.name))
            return "\(person) is in \(c.name). Without \(who), \(c.name) closes for \(person) and \(who) \(c.lost.count == 1 ? "stops" : "stop") answering there."
        }
    }

    /// The revoke confirm's extra line when the person is in rooms.
    static func revokeLine(person: String, rooms: [String]) -> String? {
        rooms.isEmpty ? nil : "\(joined(rooms)) closes for \(person), and no agent answers there until you remove \(person) from it."
    }
}
