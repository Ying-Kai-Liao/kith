import Foundation
import Testing
@testable import KithKit
@testable import KithUI

private actor Sent {
    var invites: [(name: String?, personId: String?, agents: [String: InviteRole])] = []
    func add(_ name: String?, _ personId: String?, _ agents: [String: InviteRole]) { invites.append((name, personId, agents)) }
    var access: [(personId: String, agents: [String: InviteRole]?, projects: [String]?)] = []
    func addAccess(_ personId: String, _ agents: [String: InviteRole]?, _ projects: [String]?) { access.append((personId, agents, projects)) }
}

private struct FakeHost: PeopleHost {
    let sent: Sent
    var held: [HostPerson] = []
    func people() async throws -> [HostPerson] { held }
    func invites() async throws -> [HostInvite] { [] }
    func createInvite(name: String?, personId: String?, agents: [String: InviteRole]) async throws -> CreatedInvite {
        await sent.add(name, personId, agents)
        return CreatedInvite(inviteId: "i", link: "https://example.test/join#x", expiresAt: 0, personId: personId ?? "new")
    }
    func revokeInvite(_ inviteId: String) async throws {}
    func setPersonAccess(personId: String, agents: [String: InviteRole]?, projects: [String]?) async throws {
        await sent.addAccess(personId, agents, projects)
    }
    func revokePerson(_ personId: String) async throws {}
    func unrevokePerson(_ personId: String) async throws {}
    func audit(since: Date?, personId: String?) async throws -> [AuditRow] { [] }
    func projectList() async throws -> [HQProject] { [HQProject(id: "notes")] }
}

private let helper = RoomAgent(id: "helper", name: "Helper")
private let writer = RoomAgent(id: "writer", name: "Writer")
private let studio = InviteSheetModel.Room(id: "studio", name: "Studio", agents: [helper, writer])

@MainActor
private func model(held: [HostPerson] = [], prefill: InvitePrefill = InvitePrefill(), rooms: [InviteSheetModel.Room] = [studio],
                   sent: Sent = Sent(), online: Bool = true) async -> InviteSheetModel {
    let people = PeopleStore(host: { FakeHost(sent: sent, held: held) })
    await people.load()
    await people.loadProjects()
    return InviteSheetModel(people: people, offered: [helper, writer], rooms: rooms, prefill: prefill, online: online)
}

@MainActor @Suite struct InviteSheetModelTests {
    @Test func emptyStartsWithNothingOnAndCannotCreate() async {
        let m = await model()
        #expect(m.roles.isEmpty && m.ticked.isEmpty && !m.canCreate)
        m.name = "Sam"
        #expect(!m.canCreate)
        m.setAgent("helper", on: true)
        #expect(m.canCreate)
    }

    @Test func aPresetAgentComesOnAsCanAskAndAskedByName() async {
        let m = await model(prefill: .agent("helper"))
        m.name = "  Sam "
        #expect(m.request == InviteRequest(name: "Sam", personId: nil, agents: ["helper": .guest], rooms: []))
    }

    @Test func aPresetRoomTicksItAndSwitchesItsAgentsOnAsCanAsk() async {
        let m = await model(prefill: InvitePrefill(rooms: ["studio"]))
        m.name = "Jo"
        #expect(m.request == InviteRequest(name: "Jo", personId: nil, agents: ["helper": .guest, "writer": .guest], rooms: ["studio"]))
    }

    @Test func switchingARoomsAgentOffTurnsTheRoomOffAndLevelsRideAlong() async {
        let m = await model(prefill: InvitePrefill(rooms: ["studio"]))
        m.name = "Jo"
        m.roles["helper"] = .member
        m.setAgent("writer", on: false)
        #expect(m.request.agents == ["helper": .member])
        #expect(m.request.rooms.isEmpty)
    }

    @Test func anExistingPersonSendsTheIdNeverTheNameAndKeepsWhatTheyHold() async {
        let sam = HostPerson(personId: "sam", name: "Sam", agents: ["helper": .member], joined: true)
        let m = await model(held: [sam], prefill: InvitePrefill(agents: ["writer": .guest], personId: "sam"))
        m.name = "ignored"
        #expect(m.isHeld("helper") && !m.isHeld("writer"))
        #expect(m.request == InviteRequest(name: nil, personId: "sam", agents: ["helper": .member, "writer": .guest], rooms: []))
        // A held agent can't be switched off here.
        m.setAgent("helper", on: false)
        #expect(m.roles["helper"] == .member)
    }

    @Test func createSendsTheRequestOnceAndShowsTheResult() async {
        let sent = Sent()
        let m = await model(prefill: .agent("helper"), sent: sent)
        m.name = "Sam"
        await m.create()
        await m.create()
        let invites = await sent.invites
        #expect(invites.count == 1)
        #expect(invites[0].name == "Sam" && invites[0].personId == nil && invites[0].agents == ["helper": .guest])
        #expect(m.created?.inviteId == "i")
    }

    @Test func offlineNeverCreates() async {
        let sent = Sent()
        let m = await model(prefill: .agent("helper"), sent: sent, online: false)
        m.name = "Sam"
        #expect(!m.canCreate)
        await m.create()
        #expect(await sent.invites.isEmpty)
    }

    @Test func theResultNamesTheRoomsThePersonJoins() async {
        let m = await model(prefill: InvitePrefill(rooms: ["studio"]))
        m.name = "Jo"
        await m.create()
        #expect(m.joinsLine == "Jo joins Studio when Jo accepts.")
    }

    @Test func aPersonWhoJoinedGetsAccessDirectlyNotANewLink() async {
        let sent = Sent()
        let sam = HostPerson(personId: "sam", name: "Sam", agents: ["helper": .member], projects: ["old"], joined: true)
        let m = await model(held: [sam], prefill: InvitePrefill(agents: ["writer": .guest], personId: "sam"), sent: sent)
        m.setProject("notes", on: true)
        await m.create()
        #expect(await sent.invites.isEmpty)
        let access = await sent.access
        #expect(access.count == 1)
        #expect(access[0].personId == "sam" && access[0].agents == ["helper": .member, "writer": .guest] && access[0].projects == ["old", "notes"])
        #expect(m.updated == "sam" && m.created == nil)
    }

    @Test func aPersonStillPendingGetsAFreshLink() async {
        let sent = Sent()
        let jo = HostPerson(personId: "jo", name: "Jo", agents: ["helper": .guest], joined: false)
        let m = await model(held: [jo], prefill: InvitePrefill(agents: ["writer": .guest], personId: "jo"), sent: sent)
        await m.create()
        let invites = await sent.invites
        #expect(invites.count == 1 && invites[0].personId == "jo")
        #expect(await sent.access.isEmpty)
        #expect(m.created != nil && m.updated == nil)
    }

    @Test func projectsAreOfferedAndFollowANewInviteAsAccess() async {
        let sent = Sent()
        let m = await model(prefill: .agent("helper"), sent: sent)
        #expect(m.projects == ["notes"])
        m.name = "Sam"
        m.setProject("notes", on: true)
        #expect(m.request.projects == ["notes"])
        await m.create()
        // The fake host never lists the new person, so the rooms can't be added yet: the owner is told, the link stays.
        #expect(m.created != nil)
        #expect(m.people.error?.contains("until Sam joins") == true)
    }
}
