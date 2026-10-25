import Foundation
import Testing
@testable import KithKit
@testable import KithUI

// A bot from outside's chat in the agent's roster (docs/kith-bot-guests.md 1.1, 2.5): decoded from the host's `bot`
// row, tagged in Crew, read-only, and each conversation a notice with a thread chip.

private func decodeBot(_ extra: String) throws -> Bot {
    let json = #"""
    {"id":"c1","kind":"agent","members":[],"name":"personal","description":"","avatarColor":"","avatarShape":"",
     "backend":"claude","cwd":"/w","permission":"ask","pinned":false,"hidden":false,"connectors":[],"skills":[],
     "computer":false,"createdAt":1,"rev":2,"status":"idle","activity":"","unread":0,"lastAt":3\#(extra)}
    """#
    return try JSONDecoder().decode(Bot.self, from: Data(json.utf8))
}

@Test func botGuestDecodesOnTheChatRowAndIsAbsentOtherwise() throws {
    let guest = try decodeBot(#","botGuest":{"botId":"bg","name":"Rex","provider":"grok","ownerPerson":"sam","ownerName":"Sam"}"#)
    #expect(guest.botGuest == BotGuestInfo(botId: "bg", name: "Rex", provider: "grok", ownerPerson: "sam", ownerName: "Sam"))
    #expect(guest.isReadOnly)
    let plain = try decodeBot("")
    #expect(plain.botGuest == nil && !plain.isReadOnly)
}

@Test func theTagNamesTheProviderAndWhoseBotItIs() {
    #expect(BotGuestInfo(botId: "b", name: "Rex", provider: "grok", ownerPerson: "sam", ownerName: "Sam").tag == "from Grok · Sam's Rex")
    #expect(BotGuestInfo(botId: "b", name: "Rex", provider: "grok").tag == "from Grok · Your Rex")
    #expect(BotGuestInfo(botId: "b", name: "Rex", provider: "other", ownerPerson: "sam").tag == "from Other · Someone's Rex")
}

@Test func theReadOnlyLineSaysWhoAsks() {
    #expect(BotGuestInfo(botId: "b", name: "Rex", provider: "grok").readOnlyLine == "Rex asks from Grok. You can read along here.")
}

@Test func onlyTheComputersOwnerAnswersItsCards() {
    let info = BotGuestInfo(botId: "b", name: "Rex")
    #expect(info.canAnswer(isOwner: true))
    #expect(!info.canAnswer(isOwner: false))
}

@Test func theCrewRowCarriesTheTagOnlyForABotGuestChat() throws {
    let guest = try decodeBot(#","botGuest":{"botId":"bg","name":"Rex","provider":"grok"}"#)
    #expect(CrewRowModel(id: "i", shape: .agent(CrewFace(portrait: nil)), name: "personal", tag: guest.botGuest?.tag).tag == "from Grok · Your Rex")
    #expect(CrewRowModel(id: "i", shape: .agent(CrewFace(portrait: nil)), name: "personal").tag == nil)
}

private func notice(_ thread: String?) throws -> Entry {
    let t = thread.map { #","thread":\#($0)"# } ?? ""
    let json = #"{"id":"n1","seq":1,"botId":"c1","rev":1,"kind":"notice","turn":0,"data":{"text":"from Grok · Kai's Grok","style":"info"\#(t)},"createdAt":1,"updatedAt":1}"#
    return try JSONDecoder().decode(Entry.self, from: Data(json.utf8))
}

@Test func aNoticeWithAThreadGetsAChipAndOthersDoNot() throws {
    let thread = #"{"count":2,"lastAt":5,"authors":["user"],"unread":1,"origin":"bot","originLabel":"from Grok · Kai's Grok","originBot":{"provider":"grok","owner":null,"name":"Grok"}}"#
    let with = try notice(thread)
    #expect(ChatRow.chipSummary(with, canOpen: true)?.count == 2)
    #expect(ChatRow.chipSummary(with, canOpen: false) == nil)
    #expect(ChatRow.chipSummary(try notice(nil), canOpen: true) == nil)
    #expect(ChatRow.chipSummary(try notice(#"{"count":0,"lastAt":5,"authors":[]}"#), canOpen: true) == nil)
}

@Test func aConversationRowOpensWhenTheStoreHasItsChat() throws {
    let chat = try decodeBot("")
    let row = BotGuestConversation(conversation: "cv", agent: "personal", chatBotId: "c1", threadRoot: "n1")
    #expect(BotGuestRowModel.opens(row, chats: ["c1": chat]))
    #expect(!BotGuestRowModel.opens(row, chats: [:]))
    #expect(!BotGuestRowModel.opens(BotGuestConversation(conversation: "cv", agent: "personal"), chats: ["c1": chat]))
}
