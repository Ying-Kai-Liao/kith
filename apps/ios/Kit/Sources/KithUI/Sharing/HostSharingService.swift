import KithKit
import Foundation
import Observation

/// `SharingService` on the computer's real calls, through `PeopleStore` (invites, setAccess, revoke, restore).
@MainActor
@Observable
public final class HostSharingService: SharingService {
    public let store: PeopleStore
    private let agentNames: @MainActor () -> [String]
    /// The links of invites made on this device; the host never lists them again. // backend later: a link the host returns.
    private var links: [String: String] = [:]

    public init(people: PeopleStore, agents: @escaping @MainActor () -> [String]) {
        store = people
        agentNames = agents
    }

    public var persons: [SharingPerson] {
        let open = store.pending
        let all = store.people.map { p in SharingMap.person(p, openInvites: open.filter { $0.personId == p.personId }.map(\.inviteId)) }
        return all.filter { $0.status != .removed } + all.filter { $0.status == .removed }
    }
    public var agents: [String] { Array(Set(agentNames()).union(store.people.flatMap { $0.agents.keys })).sorted() }
    public var rooms: [String] { store.projects.map(\.id) }
    public var loaded: Bool { store.loaded }
    public var unsupported: Bool { store.unsupported }
    public var busy: Bool { store.busy }
    public var error: String? { store.error }
    public var removed: SharingPerson? { store.removed.map { SharingMap.person($0, openInvites: []) } }

    public func reload() async {
        await store.load()
        await store.loadProjects()
    }

    public func share(_ what: [ShareTarget], with recipient: ShareRecipient, level: ShareLevel) async -> ShareOutcome? {
        let (agents, rooms) = SharingMap.split(what)
        switch recipient {
        case .new(let name):
            let typed = name.trimmingCharacters(in: .whitespaces)
            guard !typed.isEmpty else { return nil }
            guard !agents.isEmpty else {
                store.error = "Pick at least one agent for a new person."
                return nil
            }
            guard let made = await store.createInvite(name: typed, personId: nil, agents: Dictionary(uniqueKeysWithValues: agents.map { ($0, level.role) })) else { return nil }
            links[made.inviteId] = made.link
            // The invite carries agents only; rooms follow once the person exists.
            if !rooms.isEmpty, let id = made.personId, let person = store.person(id) {
                _ = await store.setAccess(id, agents: person.agents, projects: SharingMap.projectsAfterSharing(rooms, to: person))
            }
            return .invite(made)
        case .existing(let id):
            guard let person = store.person(id) else { return nil }
            let done: Bool
            if rooms.isEmpty {
                done = await store.writeAccess(personId: id, agents: SharingMap.agentsAfterSharing(agents, level: level, to: person))
            } else {
                done = await store.setAccess(id, agents: SharingMap.agentsAfterSharing(agents, level: level, to: person),
                                             projects: SharingMap.projectsAfterSharing(rooms, to: person))
            }
            return done ? .updated(personId: id) : nil
        }
    }

    public func setLevel(_ level: ShareLevel, of agent: String, for personId: String) async -> Bool {
        guard let person = store.person(personId), person.agents[agent] != nil else { return false }
        return await store.writeAccess(personId: personId, agents: SharingMap.agentsAfterSharing([agent], level: level, to: person))
    }

    public func unshare(_ what: ShareTarget, from personId: String) async -> Bool {
        guard let person = store.person(personId) else { return false }
        switch what {
        case .agent(let agent):
            return await store.removeAgent(agent, from: person)
        case .room(let room):
            return await store.setAccess(personId, agents: person.agents, projects: SharingMap.projectsAfterUnsharing(room, from: person))
        }
    }

    public func stopSharing(person: String) async -> Bool { await store.remove(person) }
    public func restore(person: String) async { await store.restore(person) }
    public func cancelInvite(_ inviteId: String) async { await store.revokeInvite(inviteId) }
    public func dismissRemoved() { store.dismissRemoved() }
    public func link(for inviteId: String) -> String? { links[inviteId] }
}
