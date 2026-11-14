import Foundation
import Testing
@testable import KithKit

// An invited person's side (SPEC §8-§10), and Kai's invite and settings warnings.

private let planner = RoomAgent(id: "planner", name: "planner")
private let coder = RoomAgent(id: "coder", name: "coder")
private let agents = [planner, coder]

private func entry(seq: Int64, kind: String, text: String? = nil) -> Entry {
    var data = EntryData()
    data.text = text
    return Entry(id: "e\(seq)", seq: seq, botId: "room", rev: seq, kind: kind, turn: 1, data: data, createdAt: 0, updatedAt: 0)
}

@Test func aPersonsRoomsReadAsGroupsAndCloseWhenTheyLeaveTheList() throws {
    let studio = HostContract.MyRoom(id: "room-1", name: "Studio", agents: ["planner"], stay: 7)
    let bot = try #require(MemberRoomLogic.bot(for: studio, lastAt: 5, lastMessage: "Kai: hi"))
    #expect(bot.isGroup && bot.id == "room-1" && bot.name == "Studio" && bot.members.isEmpty && bot.unread == 0)
    #expect(bot.lastMessage == "Kai: hi")
    let launch = HostContract.MyRoom(id: "room-2", name: "Launch", agents: [], stay: 9)
    #expect(MemberRoomLogic.closed(before: ["room-1", "room-2"], after: [launch]) == ["room-1"])
    #expect(MemberRoomLogic.closed(before: ["room-2"], after: [launch, studio]).isEmpty)
}

@Test func onlyTheHostsRefusalClosesARoomNotADroppedLink() {
    #expect(MemberRoomLogic.isClosedRefusal(HostError.http(403, "not in this room")))
    #expect(MemberRoomLogic.isClosedRefusal(HostError.http(404, "no such room")))
    #expect(!MemberRoomLogic.isClosedRefusal(HostError.http(500, "oops")))
    #expect(!MemberRoomLogic.isClosedRefusal(URLError(.notConnectedToInternet)))
}

@Test func aPersonsRoomWords() {
    #expect(MemberRoomLogic.startLines(room: "Studio") == ["You see Studio from when Kai added you.", "Kai sees everything said here."])
    #expect(MemberRoomLogic.closedLine(room: "Studio") == "You're no longer in Studio.")
    #expect(MemberRoomLogic.ownerLine(room: "Studio") == "Studio is Kai's room, on Kai's Mac. Kai sees everything said here.")
}

@Test func askedLineUntilAnAgentReplies() {
    #expect(MemberRoomLogic.askedLine(entries: [entry(seq: 1, kind: "user", text: "hi @planner check")], agents: ["planner", "coder"]) == "Asked planner")
    #expect(MemberRoomLogic.askedLine(entries: [entry(seq: 1, kind: "user", text: "@planner and @coder please")], agents: ["planner", "coder"]) == "Asked planner and coder")
    #expect(MemberRoomLogic.askedLine(entries: [entry(seq: 1, kind: "user", text: "hi @planner"), entry(seq: 2, kind: "agent")], agents: ["planner"]) == nil)
    #expect(MemberRoomLogic.askedLine(entries: [entry(seq: 1, kind: "user", text: "no mention")], agents: ["planner"]) == nil)
    #expect(MemberRoomLogic.askedLine(entries: [entry(seq: 1, kind: "user", text: "mail me@plannerx.com")], agents: ["planner"]) == nil)
}

@Test func theInviteSheetOffersOnlyRoomsThatTakePeopleAndStartsFromTheRoom() throws {
    func bot(_ json: String) throws -> Bot { try JSONDecoder().decode(Bot.self, from: Data(json.utf8)) }
    let a = try bot(#"{"id":"a","name":"planner","hqAgent":"planner"}"#)
    let c = try bot(#"{"id":"c","name":"coder","hqAgent":"coder"}"#)
    let studio = try bot(#"{"id":"studio","kind":"group","name":"Studio","members":["a","c"]}"#)
    let withGrok = try bot(#"{"id":"mixed","kind":"group","name":"Mixed","members":["a","grok"]}"#)
    let rooms = RoomPeopleLogic.inviteRooms(groups: [studio, withGrok, a], find: { ["a": a, "c": c][$0] })
    #expect(rooms.map(\.id) == ["studio"])
    #expect(rooms[0].agents.map(\.id) == ["planner", "coder"])
    let start = RoomPeopleLogic.roomInviteStart(agents: rooms[0].agents, room: "studio")
    #expect(start.roles == ["planner": .guest, "coder": .guest] && start.rooms == ["studio"])
}

@Test func settingsWarnBeforeARoomWouldClose() {
    let ben = HostPerson(personId: "ben", name: "Ben", agents: ["planner": .guest, "coder": .guest], joined: true)
    let inRooms = [(room: "r", name: "Studio", agents: agents)]
    let closing = RoomPeopleLogic.closingRooms(person: ben, newRoles: ["planner": .guest], inRooms: inRooms)
    #expect(closing.map(\.lost) == [[coder]])
    #expect(RoomPeopleLogic.closingLines(person: "Ben", closing: closing) == ["Ben is in Studio. Without coder, Studio closes for Ben and coder stops answering there."])
    #expect(RoomPeopleLogic.closingRooms(person: ben, newRoles: ["planner": .guest, "coder": .member], inRooms: inRooms).isEmpty)
    #expect(RoomPeopleLogic.revokeLine(person: "Ben", rooms: ["Studio"]) == "Studio closes for Ben, and no agent answers there until you remove Ben from it.")
    #expect(RoomPeopleLogic.revokeLine(person: "Ben", rooms: []) == nil)
}
