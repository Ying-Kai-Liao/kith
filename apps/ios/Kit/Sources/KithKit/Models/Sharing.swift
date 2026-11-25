import Foundation

// The owner's sharing model: "you share an agent or a room, with a person, at one of two levels". A pure view over
// what the host stores (`HostPerson` agents with roles, projects); nothing here talks to a host.

/// How far a shared agent may go. Rooms have no level: they are shared or not.
public enum ShareLevel: String, CaseIterable, Sendable {
    /// Ask and get answers (`InviteRole.guest`).
    case chat
    /// Also runs tools and code on the owner's computer (`InviteRole.member`).
    case work

    public init(role: InviteRole) { self = role == .member ? .work : .chat }

    public var role: InviteRole { self == .work ? .member : .guest }
    public var title: String { self == .work ? "Can work" : "Can chat" }
    /// "can work", for a chip after the agent's name.
    public var phrase: String { title.lowercased() }
    public var detail: String {
        self == .work ? "Also runs tools and code on your computer." : "Asks questions and gets answers. The safer start."
    }
}

/// What is shared: an agent (an hq agent) or a room (a project).
public enum ShareTarget: Hashable, Sendable {
    case agent(String)
    case room(String)

    public var name: String {
        switch self {
        case .agent(let id), .room(let id): id
        }
    }
    public var isAgent: Bool { if case .agent = self { true } else { false } }
}

/// One share a person holds.
public struct Share: Hashable, Sendable, Identifiable {
    public var what: ShareTarget
    /// Nil for a room.
    public var level: ShareLevel?
    /// Held through the everyone (`*`) row: shown, never edited per person.
    public var everyone: Bool
    public var id: String {
        switch what {
        case .agent(let a): "agent:\(a)"
        case .room(let r): "room:\(r)"
        }
    }
    public var editable: Bool { !everyone }

    public init(what: ShareTarget, level: ShareLevel? = nil, everyone: Bool = false) {
        self.what = what
        self.level = level
        self.everyone = everyone
    }

    /// "Ada · can work", "Room X", "Ada · everyone can chat".
    public var chip: String {
        switch what {
        case .room(let r): return r
        case .agent(let a):
            guard let level else { return a }
            return everyone ? "\(a) · everyone \(level.phrase)" : "\(a) · \(level.phrase)"
        }
    }
}

public enum SharingStatus: Sendable {
    case joined, waiting, removed

    public var title: String {
        switch self {
        case .joined: "Joined"
        case .waiting: "Waiting to join"
        case .removed: "Removed"
        }
    }
}

/// A person with what is shared with them.
public struct SharingPerson: Hashable, Sendable, Identifiable {
    public var id: String
    public var name: String
    public var status: SharingStatus
    public var shares: [Share]
    /// Their open invites, to cancel (and, while this device knows it, to share the link of).
    public var openInvites: [String]
    /// Their bots join without waiting (host setting); false from an older host.
    public var autoBots: Bool

    public init(id: String, name: String, status: SharingStatus, shares: [Share] = [], openInvites: [String] = [], autoBots: Bool = false) {
        self.id = id
        self.name = name
        self.status = status
        self.shares = shares
        self.openInvites = openInvites
        self.autoBots = autoBots
    }

    /// What the owner can edit: everything but the everyone rows.
    public var ownShares: [Share] { shares.filter(\.editable) }
    public func share(_ what: ShareTarget) -> Share? { shares.first { $0.what == what } }
}

/// The two-way mapping between the host's per-person agents/projects and the shares, and the bodies of the writes.
public enum SharingMap {
    /// Their shares: own agents, then everyone agents they don't hold themselves, then rooms.
    public static func shares(of person: HostPerson) -> [Share] {
        let own = person.agents.sorted { $0.key < $1.key }.map { Share(what: .agent($0.key), level: ShareLevel(role: $0.value)) }
        let everyone = person.everyoneAgents.filter { person.agents[$0.key] == nil }.sorted { $0.key < $1.key }
            .map { Share(what: .agent($0.key), level: ShareLevel(role: $0.value), everyone: true) }
        return own + everyone + person.projects.map { Share(what: .room($0)) }
    }

    public static func person(_ person: HostPerson, openInvites: [String]) -> SharingPerson {
        let status: SharingStatus = person.revoked ? .removed : person.joined ? .joined : .waiting
        return SharingPerson(id: person.personId, name: person.name, status: status, shares: shares(of: person),
                             openInvites: openInvites, autoBots: person.autoBots)
    }

    /// The `setPersonAccess` agents after sharing `agents` at `level`: their current map with those entries set.
    public static func agentsAfterSharing(_ agents: [String], level: ShareLevel, to person: HostPerson) -> [String: InviteRole] {
        var map = person.agents
        for agent in agents { map[agent] = level.role }
        return map
    }

    public static func agentsAfterUnsharing(_ agent: String, from person: HostPerson) -> [String: InviteRole] {
        person.agents.filter { $0.key != agent }
    }

    /// Their rooms after adding `rooms` (kept in order, no repeats).
    public static func projectsAfterSharing(_ rooms: [String], to person: HostPerson) -> [String] {
        person.projects + rooms.filter { !person.projects.contains($0) }
    }

    public static func projectsAfterUnsharing(_ room: String, from person: HostPerson) -> [String] {
        person.projects.filter { $0 != room }
    }

    /// Splits a pick into agents and rooms, in the order picked.
    public static func split(_ what: [ShareTarget]) -> (agents: [String], rooms: [String]) {
        (what.compactMap { if case .agent(let a) = $0 { a } else { nil } }, what.compactMap { if case .room(let r) = $0 { r } else { nil } })
    }
}
