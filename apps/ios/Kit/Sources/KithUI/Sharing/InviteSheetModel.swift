import KithKit
import Foundation
import Observation

/// What a caller already knows when it opens the invite sheet. Empty is the plain "Add a friend" from Crew, People
/// and Sharing.
public struct InvitePrefill: Equatable, Sendable {
    /// Agents switched on from the start, with their level (an agent page's Share row, a chat).
    public var agents: [String: InviteRole]
    /// Group rooms ticked from the start (a room's Add); their agents come on as Can ask.
    public var rooms: [String]
    /// Someone already invited: the sheet adds to them instead of asking for a name.
    public var personId: String?

    public init(agents: [String: InviteRole] = [:], rooms: [String] = [], personId: String? = nil) {
        self.agents = agents
        self.rooms = rooms
        self.personId = personId
    }

    /// One agent, from its page or its chat.
    public static func agent(_ id: String) -> InvitePrefill { InvitePrefill(agents: [id: .guest]) }
}

/// The arguments of one `createInvite` call.
public struct InviteRequest: Equatable, Sendable {
    public var name: String?
    public var personId: String?
    public var agents: [String: InviteRole]
    public var rooms: [String]
    /// Host projects, the second kind of room the old Sharing flow offered; applied as access, never in the invite.
    public var projects: [String] = []
}

/// The invite sheet's state: who, which agents at which level, which group rooms. The view draws it; `request` is
/// exactly what the computer is asked.
@MainActor
@Observable
public final class InviteSheetModel {
    public struct Room: Equatable, Sendable {
        public var id: String
        public var name: String
        public var agents: [RoomAgent]

        public init(id: String, name: String, agents: [RoomAgent]) {
            self.id = id
            self.name = name
            self.agents = agents
        }
    }

    public let people: PeopleStore
    /// Every agent the sheet can offer, with the name to show.
    public let offered: [RoomAgent]
    /// The group rooms that take people; the sheet refreshes them when it opens.
    public var rooms: [Room]
    public var roles: [String: InviteRole]
    public var ticked: [String]
    /// Host projects switched on (the old Sharing flow's "Rooms").
    public var projectsTicked: [String] = []
    public var name = ""
    /// Nil is a new person.
    public var personId: String?
    public private(set) var created: CreatedInvite?
    /// Set when the person had joined already: access was added directly and there is no link.
    public private(set) var updated: String?
    /// The rooms the made invite adds the person to, for the result line.
    public private(set) var createdRooms: [String] = []
    /// The computer can't be reached: Create waits and the sheet says why.
    public var online: Bool

    public init(people: PeopleStore, offered: [RoomAgent], rooms: [Room], prefill: InvitePrefill = InvitePrefill(), online: Bool = true,
                created: CreatedInvite? = nil) {
        self.created = created
        self.people = people
        self.rooms = rooms
        self.online = online
        personId = prefill.personId
        // What the person holds already is shown at its current level and rides along, so adding never takes away.
        var start = prefill.personId.flatMap { people.person($0)?.agents } ?? [:]
        for (agent, role) in prefill.agents { start[agent] = role }
        var tickedStart: [String] = []
        for id in prefill.rooms {
            guard let room = rooms.first(where: { $0.id == id }) else { continue }
            tickedStart.append(id)
            for agent in room.agents where start[agent.id] == nil { start[agent.id] = .guest }
        }
        roles = start
        ticked = tickedStart
        // A prefilled or held agent is offered even when the roster doesn't list it.
        var seen = Set(offered.map(\.id))
        var all = offered
        for id in start.keys.sorted() where seen.insert(id).inserted { all.append(RoomAgent(id: id, name: id)) }
        self.offered = all
    }

    private var held: [String: InviteRole] { personId.flatMap { people.person($0)?.agents } ?? [:] }
    /// An agent the chosen person has already: it stays on (taking one away is the person page's).
    public func isHeld(_ agent: String) -> Bool { held[agent] != nil }

    /// A person who has joined gets access directly; a pending one gets a fresh link.
    public var joined: Bool { personId.flatMap { people.person($0)?.joined } ?? false }
    /// The host projects the sheet can offer.
    public var projects: [String] { people.projects.map(\.id) }

    public var typedName: String { name.trimmingCharacters(in: .whitespaces) }
    public var who: String { personId.flatMap { people.person($0)?.name } ?? typedName }

    /// Ticked rooms whose agents are all on.
    public var followed: [String] {
        RoomPeopleLogic.followAgents(ticked: ticked, rooms: rooms.map { ($0.id, $0.agents) }, roles: roles)
    }

    public func setAgent(_ agent: String, on: Bool) {
        if on {
            roles[agent] = roles[agent] ?? .guest
        } else if !isHeld(agent) {
            roles[agent] = nil
        }
        ticked = followed
    }

    public func setRoom(_ id: String, on: Bool) {
        ticked.removeAll { $0 == id }
        if on { ticked.append(id) }
    }

    public func setProject(_ id: String, on: Bool) {
        projectsTicked.removeAll { $0 == id }
        if on { projectsTicked.append(id) }
    }

    public var canCreate: Bool {
        online && created == nil && updated == nil && !people.busy && !roles.isEmpty && (personId != nil || !typedName.isEmpty)
    }

    /// What Create sends: a name for a new person, an id for an existing one, never both.
    public var request: InviteRequest {
        InviteRequest(name: personId == nil ? typedName : nil, personId: personId, agents: roles, rooms: followed, projects: projectsTicked)
    }

    /// The first tap wins: a second while the call runs, or after it made the invite, does nothing.
    public func create() async {
        guard canCreate else { return }
        let asked = request
        if joined, let id = asked.personId, let person = people.person(id) {
            await addDirectly(asked, to: person)
            return
        }
        guard let made = await people.createInvite(name: asked.name, personId: asked.personId, agents: asked.agents, rooms: asked.rooms) else { return }
        created = made
        createdRooms = asked.rooms.compactMap { id in rooms.first { $0.id == id }?.name }
        // Projects are access, not part of the invite, so they follow it. The invite stays (its link is good); a failure
        // to add them is shown, never swallowed.
        guard !asked.projects.isEmpty else { return }
        if let id = made.personId, let person = people.person(id) {
            if await !people.setAccess(id, agents: person.agents.merging(asked.agents) { _, new in new },
                                       projects: SharingMap.projectsAfterSharing(asked.projects, to: person)) {
                people.error = "The invite is ready, but the rooms weren't added: \(people.error ?? "the computer refused")"
            }
        } else {
            people.error = "The invite is ready, but the rooms can't be added until \(who) joins."
        }
    }

    /// Someone who has joined: the access goes on them now, the way Sharing always did it.
    private func addDirectly(_ asked: InviteRequest, to person: HostPerson) async {
        let agents = person.agents.merging(asked.agents) { _, new in new }
        let done: Bool
        if asked.projects.isEmpty {
            done = await people.writeAccess(personId: person.personId, agents: agents)
        } else {
            done = await people.setAccess(person.personId, agents: agents, projects: SharingMap.projectsAfterSharing(asked.projects, to: person))
        }
        guard done else { return }
        if !asked.rooms.isEmpty, await !people.addToRooms(person.personId, rooms: asked.rooms) {
            people.error = "Access was added, but the group rooms weren't: \(people.error ?? "the computer refused")"
        }
        updated = person.personId
    }

    /// "Mia joins Studio when Mia accepts."
    public var joinsLine: String? {
        let made = created?.personId.flatMap { people.person($0)?.name } ?? who
        return RoomPeopleLogic.inviteJoinsLine(name: made, rooms: createdRooms)
    }
}
