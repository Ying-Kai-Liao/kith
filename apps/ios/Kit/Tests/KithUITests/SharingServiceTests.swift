import Foundation
import Testing
@testable import KithKit
@testable import KithUI

@Suite struct SharingMapTests {
    @Test func levelMapsToRoleBothWays() {
        #expect(ShareLevel(role: .guest) == .chat)
        #expect(ShareLevel(role: .member) == .work)
        #expect(ShareLevel.chat.role == .guest)
        #expect(ShareLevel.work.role == .member)
        #expect(ShareLevel.work.title == "Can work")
    }

    @Test func sharesListOwnAgentsThenEveryoneThenRooms() {
        let p = HostPerson(personId: "sam", name: "Sam", agents: ["ada": .member], everyoneAgents: ["coder": .guest, "ada": .guest],
                           projects: ["Launch"], joined: true)
        let shares = SharingMap.shares(of: p)
        #expect(shares.map(\.chip) == ["ada · can work", "coder · everyone can chat", "Launch"])
        #expect(shares.map(\.editable) == [true, false, true])
        #expect(shares[2].level == nil)
    }

    @Test func statusFollowsJoinedAndRevoked() {
        #expect(SharingMap.person(HostPerson(personId: "a", name: "A", joined: true), openInvites: []).status == .joined)
        #expect(SharingMap.person(HostPerson(personId: "a", name: "A", joined: false, openInvites: 1), openInvites: ["i"]).status == .waiting)
        #expect(SharingMap.person(HostPerson(personId: "a", name: "A", joined: true, revoked: true), openInvites: []).status == .removed)
    }

    @Test func changingOneShareSendsTheWholeMapWithOneEntryChanged() {
        let p = HostPerson(personId: "sam", name: "Sam", agents: ["ada": .guest, "remy": .member], everyoneAgents: ["coder": .guest], projects: ["Launch"])
        #expect(SharingMap.agentsAfterSharing(["ada"], level: .work, to: p) == ["ada": .member, "remy": .member])
        #expect(SharingMap.agentsAfterSharing(["new"], level: .chat, to: p) == ["ada": .guest, "remy": .member, "new": .guest])
        #expect(SharingMap.agentsAfterUnsharing("remy", from: p) == ["ada": .guest])
        #expect(SharingMap.projectsAfterSharing(["Launch", "Studio"], to: p) == ["Launch", "Studio"])
        #expect(SharingMap.projectsAfterUnsharing("Launch", from: p).isEmpty)
    }

    @Test func splitSeparatesAgentsFromRooms() {
        let s = SharingMap.split([.agent("ada"), .room("Launch"), .agent("remy")])
        #expect(s.agents == ["ada", "remy"])
        #expect(s.rooms == ["Launch"])
    }
}

private actor Log {
    var invites: [(name: String?, personId: String?, agents: [String: InviteRole])] = []
    var access: [(String, [String: InviteRole]?, [String]?)] = []
    var revoked: [String] = []
    var restored: [String] = []
    var cancelled: [String] = []
    func invite(_ n: String?, _ p: String?, _ a: [String: InviteRole]) { invites.append((n, p, a)) }
    func access(_ id: String, _ a: [String: InviteRole]?, _ p: [String]?) { access.append((id, a, p)) }
    func revoke(_ id: String) { revoked.append(id) }
    func restore(_ id: String) { restored.append(id) }
    func cancel(_ id: String) { cancelled.append(id) }
}

private struct FakeHost: PeopleHost {
    let log: Log
    var persons: [HostPerson] = []
    func people() async throws -> [HostPerson] { persons }
    func invites() async throws -> [HostInvite] { [] }
    func createInvite(name: String?, personId: String?, agents: [String: InviteRole]) async throws -> CreatedInvite {
        await log.invite(name, personId, agents)
        return CreatedInvite(inviteId: "i1", link: "https://kith.example/join#t", expiresAt: 0, personId: "new")
    }
    func revokeInvite(_ inviteId: String) async throws { await log.cancel(inviteId) }
    func setPersonAccess(personId: String, agents: [String: InviteRole]?, projects: [String]?) async throws { await log.access(personId, agents, projects) }
    func revokePerson(_ personId: String) async throws { await log.revoke(personId) }
    func unrevokePerson(_ personId: String) async throws { await log.restore(personId) }
    func audit(since: Date?, personId: String?) async throws -> [AuditRow] { [] }
    func projectList() async throws -> [HQProject] { [HQProject(id: "Launch")] }
}

private let sam = HostPerson(personId: "sam", name: "Sam", agents: ["ada": .guest, "remy": .member], everyoneAgents: ["coder": .guest],
                             projects: ["Launch"], joined: true)

@MainActor private func service(_ persons: [HostPerson] = [sam]) async -> (HostSharingService, Log) {
    let log = Log()
    let people = PeopleStore(host: { FakeHost(log: log, persons: persons) })
    let service = HostSharingService(people: people, agents: { ["ada", "remy"] })
    await service.reload()
    return (service, log)
}

@MainActor @Suite struct HostSharingServiceTests {
    @Test func listsPeopleWithTheirSharesAndRooms() async {
        let (s, _) = await service()
        #expect(s.persons.map(\.id) == ["sam"])
        #expect(s.persons[0].shares.map(\.chip) == ["ada · can chat", "remy · can work", "coder · everyone can chat", "Launch"])
        #expect(s.rooms == ["Launch"])
        #expect(s.agents == ["ada", "remy"])
        #expect(s.summary == "Shared with 1 person")
    }

    @Test func newPersonAtCanChatMakesAGuestInvite() async {
        let (s, log) = await service([])
        let outcome = await s.share([.agent("ada")], with: .new(name: "  Mia "), level: .chat)
        guard case .invite(let made)? = outcome else { Issue.record("expected an invite"); return }
        #expect(made.link == "https://kith.example/join#t")
        let invites = await log.invites
        #expect(invites.count == 1)
        #expect(invites[0].name == "Mia")
        #expect(invites[0].personId == nil)
        #expect(invites[0].agents == ["ada": .guest])
        #expect(s.link(for: "i1") == made.link)
    }

    @Test func newPersonWithRoomsShowsWhyTheRoomsWaitInsteadOfSilentlyDroppingThem() async {
        let (s, log) = await service([])
        let outcome = await s.share([.agent("ada"), .room("Launch")], with: .new(name: "Mia"), level: .chat)
        guard case .invite? = outcome else { Issue.record("expected an invite"); return }
        #expect(await log.invites.count == 1)
        #expect(await log.access.isEmpty)
        #expect(s.error?.contains("rooms can't be added until Mia joins") == true)
    }

    @Test func newPersonNeedsAnAgent() async {
        let (s, log) = await service([])
        #expect(await s.share([.room("Launch")], with: .new(name: "Mia"), level: .chat) == nil)
        #expect(await s.share([.agent("ada")], with: .new(name: "  "), level: .chat) == nil)
        #expect(await log.invites.isEmpty)
    }

    @Test func existingPersonGetsTheAgentThroughSetAccessKeepingTheRest() async {
        let (s, log) = await service()
        let outcome = await s.share([.agent("coder")], with: .existing("sam"), level: .work)
        guard case .updated(let id)? = outcome else { Issue.record("expected updated"); return }
        #expect(id == "sam")
        let access = await log.access
        #expect(access.count == 1)
        #expect(access[0].1 == ["ada": .guest, "remy": .member, "coder": .member])
        #expect(access[0].2 == nil)
        #expect(await log.invites.isEmpty)
    }

    @Test func sharingARoomSendsRoomsAlongWithTheCurrentAgents() async {
        let (s, log) = await service()
        _ = await s.share([.room("Studio")], with: .existing("sam"), level: .chat)
        let access = await log.access
        #expect(access[0].1 == ["ada": .guest, "remy": .member])
        #expect(access[0].2 == ["Launch", "Studio"])
    }

    @Test func setLevelChangesOneAgentOnly() async {
        let (s, log) = await service()
        #expect(await s.setLevel(.work, of: "ada", for: "sam"))
        let access = await log.access
        #expect(access[0].1 == ["ada": .member, "remy": .member])
        #expect(access[0].2 == nil)
        // An everyone-only agent is not theirs to edit.
        #expect(await s.setLevel(.work, of: "coder", for: "sam") == false)
        #expect(await log.access.count == 1)
    }

    @Test func unshareRemovesOneAgentOrOneRoom() async {
        let (s, log) = await service()
        #expect(await s.unshare(.agent("remy"), from: "sam"))
        #expect(await s.unshare(.room("Launch"), from: "sam"))
        let access = await log.access
        #expect(access[0].1 == ["ada": .guest])
        #expect(access[0].2 == nil)
        #expect(access[1].1 == ["ada": .guest, "remy": .member])
        #expect(access[1].2 == [])
        #expect(await log.revoked.isEmpty)
    }

    @Test func stopSharingRevokesAndRestoreUndoes() async {
        let (s, log) = await service()
        #expect(await s.stopSharing(person: "sam"))
        #expect(await log.revoked == ["sam"])
        #expect(s.removed?.id == "sam")
        await s.restore(person: "sam")
        #expect(await log.restored == ["sam"])
        #expect(s.removed == nil)
    }

    @Test func cancelInviteRevokesIt() async {
        let (s, log) = await service()
        await s.cancelInvite("i9")
        #expect(await log.cancelled == ["i9"])
    }

    @Test func holdersOfAnAgentIncludeEveryoneRowsButNotRemovedPeople() async {
        let gone = HostPerson(personId: "gone", name: "Gone", agents: ["ada": .guest], joined: true, revoked: true)
        let (s, _) = await service([sam, gone])
        let ada = s.holders(of: "ada")
        #expect(ada.map(\.person.id) == ["sam"])
        let coder = s.holders(of: "coder")
        #expect(coder.map(\.person.id) == ["sam"])
        #expect(coder[0].share.everyone)
        #expect(s.persons.map(\.id) == ["sam", "gone"])
    }
}

@MainActor @Suite struct MockSharingServiceTests {
    @Test func sampleShowsJoinedAndWaitingPeople() {
        let s = MockSharingService()
        #expect(s.persons.map(\.status) == [.joined, .joined, .waiting])
        #expect(s.persons[2].openInvites == ["inv-lee"])
        #expect(s.link(for: "inv-lee") != nil)
    }

    @Test func sharingUnsharingAndStoppingWorkInMemory() async {
        let s = MockSharingService()
        guard case .invite(let made)? = await s.share([.agent("remy")], with: .new(name: "Mia"), level: .work) else { Issue.record("no invite"); return }
        #expect(s.person(made.personId ?? "")?.shares.map(\.chip) == ["remy · can work"])
        #expect(await s.setLevel(.chat, of: "ada", for: "sam"))
        #expect(s.person("sam")?.share(.agent("ada"))?.level == .chat)
        #expect(await s.unshare(.room("Launch"), from: "sam"))
        #expect(s.person("sam")?.share(.room("Launch")) == nil)
        #expect(await s.stopSharing(person: "maya"))
        #expect(s.persons.last?.id == "maya")
        await s.restore(person: "maya")
        #expect(s.person("maya")?.status == .joined)
        await s.cancelInvite("inv-lee")
        #expect(s.person("lee")?.openInvites.isEmpty == true)
    }
}
