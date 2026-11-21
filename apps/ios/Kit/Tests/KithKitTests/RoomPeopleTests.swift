import Foundation
import Testing
@testable import KithKit

// The same rules as desktop's shared/room-people.test.ts.

private let planner = RoomAgent(id: "planner", name: "planner")
private let coder = RoomAgent(id: "coder", name: "coder")
private let agents = [planner, coder]

private func person(_ id: String, _ name: String, _ held: [String], joined: Bool = true, revoked: Bool = false, everyone: [String] = []) -> HostPerson {
    HostPerson(personId: id, name: name, agents: Dictionary(uniqueKeysWithValues: held.map { ($0, InviteRole.guest) }),
               everyoneAgents: Dictionary(uniqueKeysWithValues: everyone.map { ($0, InviteRole.guest) }), joined: joined, revoked: revoked)
}

private func stay(_ person: String, _ joined: Int64, left: Int64? = nil) -> RoomStay { RoomStay(person: person, joinedGen: joined, leftGen: left) }

private func entry(seq: Int64, kind: String, _ edit: (inout EntryData) -> Void = { _ in }) -> Entry {
    var data = EntryData()
    edit(&data)
    return Entry(id: "e\(seq)", seq: seq, botId: "room", rev: seq, kind: kind, turn: 1, data: data, createdAt: 0, updatedAt: 0)
}

@Test func pickerGroupsPeopleByWhetherTheyHoldEveryRoomAgent() {
    let ben = person("ben", "Ben", ["planner", "coder"])
    let g = RoomPeopleLogic.pickerGroups(
        people: [ben, person("ann", "Ann", ["planner"]), person("mia", "Mia", [], joined: false), person("rev", "Rev", ["planner", "coder"], revoked: true), person("zed", "Zed", ["planner", "coder"])],
        stays: [stay("ben", 5)], agents: agents)
    #expect(g.canJoin.map(\.id) == ["zed"])
    #expect(g.needsAccess.map(\.id) == ["ann", "mia"])
    #expect(g.needsAccess[0].missing == [coder])
    #expect(g.needsAccess[1].accepted == false)
    // Everyone's agents count as held.
    #expect(RoomPeopleLogic.pickerGroups(people: [person("p", "P", ["planner"], everyone: ["coder"])], stays: [], agents: agents).canJoin.count == 1)
}

@Test func needsKaiRowsAndTheWontAnswerLine() {
    let ann = person("ann", "Ann", ["planner"])
    let ben = person("ben", "Ben", ["planner", "coder"])
    let stays = [stay("ann", 3), stay("ben", 4), stay("old", 1, left: 2)]
    let rows = RoomPeopleLogic.needsKai(stays: stays, people: [ann, ben, person("old", "Old", [])], agents: agents)
    #expect(rows.map(\.personId) == ["ann"])
    #expect(RoomPeopleLogic.wontAnswerLine(rows, agents: agents) == "coder won't answer here. Ann doesn't have coder.")
    #expect(RoomPeopleLogic.inTheRoom(stays: stays, people: [ann, ben], agents: agents).map(\.personId) == ["ben"])

    let cy = person("cy", "Cy", [])
    let two = RoomPeopleLogic.needsKai(stays: [stay("ann", 1), stay("cy", 1)], people: [ann, cy], agents: agents)
    #expect(RoomPeopleLogic.wontAnswerLine(two, agents: agents) == "planner and coder won't answer here. Ann and Cy are missing access.")

    var revoked = ben
    revoked.revoked = true
    let rev = RoomPeopleLogic.needsKai(stays: [stay("ben", 1)], people: [revoked], agents: agents)
    #expect(RoomPeopleLogic.wontAnswerLine(rev, agents: agents) == "No agent answers here while Ben's access is revoked.")
    #expect(RoomPeopleLogic.wontAnswerLine([], agents: agents) == nil)
}

@Test func theHiddenMarkerShowsOnlyBelowThePersonsJoinLine() {
    let stays = [stay("ben", 10), stay("ann", 20)]
    let name: (String) -> String = { $0 == "ben" ? "Ben" : "Ann" }
    func hidden(_ seq: Int64, _ ids: [String]) -> Entry { entry(seq: seq, kind: "agent") { $0.hiddenFrom = ids } }
    #expect(RoomPeopleLogic.hiddenFromNames(hidden(9, ["ben"]), stays: stays, nameOf: name) == [])
    #expect(RoomPeopleLogic.hiddenFromNames(hidden(11, ["ben"]), stays: stays, nameOf: name) == ["Ben"])
    #expect(RoomPeopleLogic.hiddenFromNames(hidden(25, ["ben", "ann"]), stays: stays, nameOf: name) == ["Ben", "Ann"])
    #expect(RoomPeopleLogic.hiddenFromNames(entry(seq: 25, kind: "user"), stays: stays, nameOf: name) == [])
    #expect(RoomPeopleLogic.hiddenMarkerText(["Ben"]) == "From before Ben joined")
    #expect(RoomPeopleLogic.hiddenMarkerText(["Ben", "Ann"]) == "From before Ben and Ann joined")
    #expect(RoomPeopleLogic.hiddenMarkerText(["Ben", "Ann", "Cy"]) == "Hidden from 3 people")
}

@Test func joinLinesAndTheWaitingNotice() throws {
    let join1 = entry(seq: 4, kind: "notice") { $0.notice = "personJoined"; $0.person = EntryPerson(id: "ben", name: "Ben") }
    let join2 = entry(seq: 9, kind: "notice") { $0.notice = "personJoined"; $0.person = EntryPerson(id: "ann", name: "Ann") }
    #expect(RoomPeopleLogic.isFirstJoin(join1, in: [join1, join2]))
    #expect(!RoomPeopleLogic.isFirstJoin(join2, in: [join1, join2]))
    #expect(RoomPeopleLogic.joinLines(join1, firstJoin: true) == ["Ben joined · sees from here", "Agents now answer only when @'d."])
    #expect(RoomPeopleLogic.joinLines(join2, firstJoin: false) == ["Ann joined · sees from here"])
    let left = entry(seq: 12, kind: "notice") { $0.notice = "personLeft"; $0.person = EntryPerson(id: "ben", name: "Ben") }
    #expect(RoomPeopleLogic.joinLines(left, firstJoin: false) == ["Ben left"])
    let waiting = entry(seq: 13, kind: "notice") { $0.text = "planner is waiting for the owner to answer a card." }
    #expect(RoomPeopleLogic.waitingAgent(waiting) == "planner")
    #expect(RoomPeopleLogic.waitingAgent(entry(seq: 14, kind: "notice") { $0.text = "other" }) == nil)
}

@Test func theInviteSheetRoomSwitchesFollowTheAgentSwitches() {
    let rooms = [(id: "room-1", agents: agents)]
    #expect(RoomPeopleLogic.followAgents(ticked: ["room-1"], rooms: rooms, roles: ["planner": .guest]) == [])
    #expect(RoomPeopleLogic.followAgents(ticked: ["room-1"], rooms: rooms, roles: ["planner": .guest, "coder": .guest]) == ["room-1"])
    #expect(RoomPeopleLogic.inviteJoinsLine(name: "Mia", rooms: ["Studio", "Launch"]) == "Mia joins Studio and Launch when Mia accepts.")
    #expect(RoomPeopleLogic.inviteJoinsLine(name: "Mia", rooms: []) == nil)
}

@Test func invitedRowsAndEarlierPeople() {
    func invite(_ edit: (inout HostInvite) -> Void) -> HostInvite {
        var i = HostInvite(inviteId: "i", personId: "mia", agents: [:], createdAt: 1, expiresAt: 1000, usedAt: nil, revokedAt: nil)
        i.rooms = [InviteRoom(room: "r", done: false)]
        edit(&i)
        return i
    }
    let mia = person("mia", "Mia", [], joined: false)
    func row(_ i: HostInvite, now: Int64 = 10) -> InvitedRow? {
        RoomPeopleLogic.invitedRows(invites: [i], people: [mia], room: "r", agents: agents, now: Date(timeIntervalSince1970: Double(now) / 1000)).first
    }
    #expect(row(invite { _ in })?.state == .waiting)
    #expect(row(invite { $0.usedAt = 5 })?.state == .accepted)
    #expect(row(invite { $0.usedAt = 5 })?.missing == agents)
    #expect(row(invite { _ in }, now: 2000)?.state == .ranOut)
    #expect(row(invite { $0.revokedAt = 3 })?.state == .ranOut)
    #expect(row(invite { $0.rooms = [InviteRoom(room: "r", done: true)] }) == nil)
    #expect(row(invite { $0.rooms = [InviteRoom(room: "other", done: false)] }) == nil)
    let ben = person("ben", "Ben", ["planner", "coder"])
    #expect(RoomPeopleLogic.earlier(stays: [stay("ben", 1, left: 2), stay("ann", 1)], people: [ben, person("ann", "Ann", [])]).map(\.personId) == ["ben"])
}

@Test func aRoomWithAnOutsideBotTakesNoPeople() {
    #expect(RoomPeopleLogic.block(agents: agents, outside: ["Grok"]) == .outsideBot("Grok"))
    #expect(RoomPeopleLogic.block(agents: agents, outside: []) == nil)
    #expect(RoomPeopleLogic.block(agents: [], outside: []) == .noAgents)
    #expect(RoomBlock.outsideBot("Grok").text(room: "Studio") == "People can't join a room with an outside bot. Remove Grok from Studio first.")
}

@Test func entryPersonReadsAnIdOrAnObjectAndHiddenFromComesThrough() throws {
    let card = try JSONDecoder().decode(EntryData.self, from: Data(#"{"person":"sam","room":"studio"}"#.utf8))
    #expect(card.person?.id == "sam")
    #expect(card.room == "studio")
    let join = try JSONDecoder().decode(EntryData.self, from: Data(#"{"notice":"personJoined","person":{"id":"ben","name":"Ben"},"hiddenFrom":["ben"],"unanswered":true}"#.utf8))
    #expect(join.person?.name == "Ben")
    #expect(join.hiddenFrom == ["ben"])
    #expect(join.unanswered == true)
    let invite = try JSONDecoder().decode(HostInvite.self, from: Data(#"{"inviteId":"i","personId":"p","agents":{},"createdAt":1,"expiresAt":2,"rooms":[{"room":"r","done":true}]}"#.utf8))
    #expect(invite.rooms == [InviteRoom(room: "r", done: true)])
}
