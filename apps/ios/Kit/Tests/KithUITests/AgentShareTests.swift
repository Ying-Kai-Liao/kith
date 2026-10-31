import Foundation
import Testing
@testable import KithKit
@testable import KithUI

private func person(_ id: String, _ agents: [String: InviteRole]) -> HostPerson {
    HostPerson(personId: id, name: id, agents: agents)
}

private func invite(_ id: String, agents: [String: InviteRole], expiresIn: TimeInterval = 3600, used: Bool = false) -> HostInvite {
    HostInvite(inviteId: id, personId: "p", agents: agents, createdAt: 0,
               expiresAt: Int64((Date.now.timeIntervalSince1970 + expiresIn) * 1000), usedAt: used ? 1 : nil, revokedAt: nil)
}

@Suite struct AgentShareTests {
    @Test func sharedWithListsOnlyPeopleWhoHaveTheAgent() {
        let people = [person("ann", ["coder": .member]), person("bo", ["writer": .guest]), person("cy", ["coder": .guest, "writer": .guest])]
        let rows = AgentShare.sharedWith("coder", in: people)
        #expect(rows.map(\.person.personId) == ["ann", "cy"])
        #expect(rows.map(\.role) == [.member, .guest])
    }

    @Test func openInvitesSkipUsedExpiredAndOtherAgents() {
        let invites = [invite("a", agents: ["coder": .guest]),
                       invite("b", agents: ["coder": .guest], used: true),
                       invite("c", agents: ["coder": .guest], expiresIn: -10),
                       invite("d", agents: ["writer": .guest])]
        #expect(AgentShare.openInvites(for: "coder", in: invites).map(\.inviteId) == ["a"])
    }

    @Test func inviteBodyNamesOnlyThisAgent() {
        #expect(AgentShare.inviteAgents("coder", role: .guest) == ["coder": .guest])
        #expect(AgentShare.inviteAgents("coder", role: .member) == ["coder": .member])
    }

    @Test func removingKeepsTheirOtherAgentsAndRoles() {
        let p = person("ann", ["coder": .member, "writer": .guest])
        #expect(AgentShare.agentsAfterRemoving("coder", from: p) == ["writer": .guest])
        #expect(AgentShare.agentsAfterRemoving("coder", from: person("bo", ["coder": .guest])).isEmpty)
    }

    @Test func onlyAnHqAgentCanBeShared() {
        #expect(AgentShare.canShare(hqAgent: "coder"))
        #expect(!AgentShare.canShare(hqAgent: ""))
        #expect(!AgentShare.canShare(hqAgent: nil))
    }
}

private actor Calls {
    var access: [(String, [String: InviteRole]?, [String]?)] = []
    var revoked: [String] = []
    func access(_ id: String, _ a: [String: InviteRole]?, _ p: [String]?) { access.append((id, a, p)) }
    func revoke(_ id: String) { revoked.append(id) }
}

private struct FakeHost: PeopleHost {
    let calls: Calls
    func people() async throws -> [HostPerson] { [] }
    func invites() async throws -> [HostInvite] { [] }
    func createInvite(name: String?, personId: String?, agents: [String: InviteRole]) async throws -> CreatedInvite {
        CreatedInvite(inviteId: "i", link: "l", expiresAt: 0, personId: personId)
    }
    func revokeInvite(_ inviteId: String) async throws {}
    func setPersonAccess(personId: String, agents: [String: InviteRole]?, projects: [String]?) async throws {
        await calls.access(personId, agents, projects)
    }
    func revokePerson(_ personId: String) async throws { await calls.revoke(personId) }
    func unrevokePerson(_ personId: String) async throws {}
    func audit(since: Date?, personId: String?) async throws -> [AuditRow] { [] }
    func projectList() async throws -> [HQProject] { [] }
}

@MainActor @Suite struct AgentShareRemoveTests {
    @Test func removeAgentWritesOtherAgentsWithoutProjectsAndNeverRevokesThePerson() async {
        let calls = Calls()
        let store = PeopleStore(host: { FakeHost(calls: calls) })
        let p = HostPerson(personId: "ann", name: "Ann", agents: ["coder": .member, "writer": .guest], projects: ["Launch"])
        let ok = await store.removeAgent("coder", from: p)
        #expect(ok)
        let access = await calls.access
        #expect(access.count == 1)
        #expect(access[0].0 == "ann")
        #expect(access[0].1 == ["writer": .guest])
        #expect(access[0].2 == nil)
        #expect(await calls.revoked.isEmpty)
    }
}
