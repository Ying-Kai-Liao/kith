import KithKit
import Foundation
import Observation

/// `SharingService` in memory, with sample people: previews, the demo and tests. Uses the same mapping as the host one.
@MainActor
@Observable
public final class MockSharingService: SharingService {
    public private(set) var hostPeople: [HostPerson]
    /// inviteId -> personId, for open invites.
    public private(set) var invites: [String: String]
    public let agents: [String]
    public let rooms: [String]
    public private(set) var removed: SharingPerson?
    public var error: String?
    public private(set) var madeInvites = 0

    public init(people: [HostPerson]? = nil, invites: [String: String]? = nil,
                agents: [String] = ["ada", "remy", "coder"], rooms: [String] = ["Launch", "Studio"]) {
        hostPeople = people ?? MockSharingService.sample
        self.invites = invites ?? (people == nil ? ["inv-lee": "lee"] : [:])
        self.agents = agents
        self.rooms = rooms
    }

    public static var sample: [HostPerson] {
        [HostPerson(personId: "maya", name: "Maya", agents: ["ada": .guest], joined: true),
         HostPerson(personId: "sam", name: "Sam", agents: ["ada": .member, "remy": .guest], everyoneAgents: ["coder": .guest],
                    projects: ["Launch"], joined: true),
         HostPerson(personId: "lee", name: "Lee", agents: ["ada": .guest], joined: false, openInvites: 1)]
    }

    public var persons: [SharingPerson] {
        let all = hostPeople.map { p in SharingMap.person(p, openInvites: invites.filter { $0.value == p.personId }.keys.sorted()) }
        return all.filter { $0.status != .removed } + all.filter { $0.status == .removed }
    }
    public var loaded: Bool { true }
    public var unsupported: Bool { false }
    public var busy: Bool { false }

    public func reload() async {}

    private func index(_ id: String) -> Int? { hostPeople.firstIndex { $0.personId == id } }

    public func share(_ what: [ShareTarget], with recipient: ShareRecipient, level: ShareLevel) async -> ShareOutcome? {
        let (agents, rooms) = SharingMap.split(what)
        removed = nil
        switch recipient {
        case .new(let name):
            let typed = name.trimmingCharacters(in: .whitespaces)
            guard !typed.isEmpty, !agents.isEmpty else { return nil }
            madeInvites += 1
            let id = "new\(madeInvites)"
            hostPeople.append(HostPerson(personId: id, name: typed, agents: Dictionary(uniqueKeysWithValues: agents.map { ($0, level.role) }),
                                         projects: rooms, joined: false, openInvites: 1))
            invites["inv-\(id)"] = id
            let json = #"{"inviteId":"inv-\#(id)","link":"https://kith.app/join#demo-\#(id)","expiresAt":0,"personId":"\#(id)"}"#
            return (try? JSONDecoder().decode(CreatedInvite.self, from: Data(json.utf8))).map { .invite($0) }
        case .existing(let id):
            guard let i = index(id) else { return nil }
            let person = hostPeople[i]
            hostPeople[i].agents = SharingMap.agentsAfterSharing(agents, level: level, to: person)
            hostPeople[i].projects = SharingMap.projectsAfterSharing(rooms, to: person)
            return .updated(personId: id)
        }
    }

    public func setLevel(_ level: ShareLevel, of agent: String, for person: String) async -> Bool {
        guard let i = index(person), hostPeople[i].agents[agent] != nil else { return false }
        hostPeople[i].agents = SharingMap.agentsAfterSharing([agent], level: level, to: hostPeople[i])
        return true
    }

    public func unshare(_ what: ShareTarget, from person: String) async -> Bool {
        guard let i = index(person) else { return false }
        switch what {
        case .agent(let a): hostPeople[i].agents = SharingMap.agentsAfterUnsharing(a, from: hostPeople[i])
        case .room(let r): hostPeople[i].projects = SharingMap.projectsAfterUnsharing(r, from: hostPeople[i])
        }
        return true
    }

    public func stopSharing(person: String) async -> Bool {
        guard let i = index(person) else { return false }
        hostPeople[i].revoked = true
        removed = persons.first { $0.id == person }
        return true
    }

    public func restore(person: String) async {
        guard let i = index(person) else { return }
        hostPeople[i].revoked = false
        if removed?.id == person { removed = nil }
    }

    public func cancelInvite(_ inviteId: String) async { invites[inviteId] = nil }
    public func dismissRemoved() { removed = nil }
    public func link(for inviteId: String) -> String? { invites[inviteId].map { "https://kith.app/join#demo-\($0)" } }
}
