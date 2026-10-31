import KithKit
import Foundation
import Observation

/// What the People screens ask of a computer; `HostClient` answers it, a fake does in tests.
public protocol PeopleHost: Sendable {
    func people() async throws -> [HostPerson]
    func invites() async throws -> [HostInvite]
    func createInvite(name: String?, personId: String?, agents: [String: InviteRole]) async throws -> CreatedInvite
    func revokeInvite(_ inviteId: String) async throws
    func setPersonAccess(personId: String, agents: [String: InviteRole]?, projects: [String]?) async throws
    func revokePerson(_ personId: String) async throws
    func unrevokePerson(_ personId: String) async throws
    func audit(since: Date?, personId: String?) async throws -> [AuditRow]
    func projectList() async throws -> [HQProject]
}

extension HostClient: PeopleHost {
    public func projectList() async throws -> [HQProject] { try await projects().projects }
}

/// The owner's view of the people invited to a computer: who they are, what they may use, what is pending.
@MainActor
@Observable
public final class PeopleStore {
    public private(set) var people: [HostPerson] = []
    public private(set) var invites: [HostInvite] = []
    public private(set) var audit: [AuditRow] = []
    public private(set) var projects: [HQProject] = []
    /// The computer's host doesn't know invites (older than #95): the People screens stay away.
    public private(set) var unsupported = false
    public private(set) var loaded = false
    /// A write is in flight: Create and the edit buttons wait, so a double tap makes one invite.
    public private(set) var busy = false
    public var error: String?
    /// Who the owner just removed, so the page can offer Undo until they do something else.
    public private(set) var removed: HostPerson?

    private let host: @MainActor () -> (any PeopleHost)?

    public init(host: @escaping @MainActor () -> (any PeopleHost)?) {
        self.host = host
    }

    public convenience init(store: BotStore) {
        self.init(host: { [weak store] in store?.client })
    }

    /// Invites still waiting for the person.
    public var pending: [HostInvite] { invites.filter { $0.isOpen() } }

    public func person(_ id: String) -> HostPerson? { people.first { $0.personId == id } }

    private func note(_ error: Error) {
        if InviteProof.isUnknownMethod(error) {
            unsupported = true
        } else {
            self.error = error.localizedDescription
        }
    }

    public func load() async {
        guard let host = host() else { return }
        error = nil
        do {
            async let p = host.people()
            async let i = host.invites()
            let (people, invites) = try await (p, i)
            self.people = people
            self.invites = invites
            loaded = true
        } catch {
            note(error)
        }
    }

    public func loadProjects() async {
        guard projects.isEmpty, let host = host() else { return }
        // Only a convenience for the edit screen; a computer without projects just shows none.
        projects = (try? await host.projectList()) ?? []
    }

    public func loadAudit(personId: String? = nil) async {
        guard let host = host() else { return }
        error = nil
        do {
            audit = try await host.audit(since: nil, personId: personId)
        } catch {
            note(error)
        }
    }

    /// Makes an invite for a new person (`name`) or one already invited (`personId`). Nil when it failed (see `error`)
    /// or while another write is still running.
    public func createInvite(name: String?, personId: String?, agents: [String: InviteRole]) async -> CreatedInvite? {
        guard !busy, let host = host() else { return nil }
        busy = true
        error = nil
        defer { busy = false }
        do {
            let made = try await host.createInvite(name: name, personId: personId, agents: agents)
            await load()
            return made
        } catch {
            note(error)
            return nil
        }
    }

    public func revokeInvite(_ id: String) async {
        await write { try await $0.revokeInvite(id) }
    }

    public func setAccess(_ personId: String, agents: [String: InviteRole], projects: [String]) async -> Bool {
        await write { try await $0.setPersonAccess(personId: personId, agents: agents, projects: projects) }
    }

    func writeAccess(personId: String, agents: [String: InviteRole]) async -> Bool {
        await write { try await $0.setPersonAccess(personId: personId, agents: agents, projects: nil) }
    }

    /// Ends their access. Their chats and memory stay on the computer.
    public func remove(_ personId: String) async -> Bool {
        let gone = person(personId)
        let done = await write { try await $0.revokePerson(personId) }
        if done { removed = gone }
        return done
    }

    /// Undo of a remove, from the banner or from the revoked person's row. The host's reason lands in `error`.
    public func restore(_ personId: String) async {
        if await write({ try await $0.unrevokePerson(personId) }), removed?.personId == personId { removed = nil }
    }

    public func dismissRemoved() { removed = nil }

    @discardableResult
    private func write(_ call: (any PeopleHost) async throws -> Void) async -> Bool {
        guard !busy, let host = host() else { return false }
        busy = true
        error = nil
        defer { busy = false }
        do {
            try await call(host)
            await load()
            return true
        } catch {
            note(error)
            return false
        }
    }
}

/// What sharing one agent means: who has it, which invites offer it, and the bodies that touch only it.
public enum AgentShare {
    /// Only a bot that is an hq agent can be shared; the invite names that agent.
    public static func canShare(hqAgent: String?) -> Bool { !(hqAgent ?? "").isEmpty }

    /// People who have the agent, with the role they have for it.
    public static func sharedWith(_ agent: String, in people: [HostPerson]) -> [(person: HostPerson, role: InviteRole)] {
        people.compactMap { person in person.agents[agent].map { (person, $0) } }
    }

    /// Invites still waiting that offer the agent.
    public static func openInvites(for agent: String, in invites: [HostInvite], at now: Date = .now) -> [HostInvite] {
        invites.filter { $0.isOpen(at: now) && $0.agents[agent] != nil }
    }

    /// The invite body: this agent only, never the person's other agents.
    public static func inviteAgents(_ agent: String, role: InviteRole) -> [String: InviteRole] { [agent: role] }

    /// The `setPersonAccess` agents for removing the agent: everything else they have, unchanged.
    public static func agentsAfterRemoving(_ agent: String, from person: HostPerson) -> [String: InviteRole] {
        person.agents.filter { $0.key != agent }
    }
}

extension PeopleStore {
    /// Takes this one agent away from the person. They stay listed, with whatever else they have; rooms are kept
    /// (`projects` omitted).
    public func removeAgent(_ agent: String, from person: HostPerson) async -> Bool {
        await writeAccess(personId: person.personId, agents: AgentShare.agentsAfterRemoving(agent, from: person))
    }
}
