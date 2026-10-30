import Foundation
import Testing
@testable import KithKit

private func decode<T: Decodable>(_ json: String) throws -> T { try JSONDecoder().decode(T.self, from: Data(json.utf8)) }

private func conversation(_ id: String = "cv1", state: String = "open", why: String? = nil, left: Int = 6, budget: Int = 10,
                          current: String? = nil, waitUntil: Int64? = nil, lane: String? = nil) throws -> RoomConversation {
    var parts = [#""conversation":"\#(id)""#, #""chat":"room""#, #""budget":\#(budget)"#, #""left":\#(left)"#, #""state":"\#(state)""#, #""opening":"o1""#]
    if let why { parts.append(#""endedWhy":"\#(why)""#) }
    if let current { parts.append(#""current":\#(current)"#) }
    if let waitUntil { parts.append(#""waitUntil":\#(waitUntil)"#) }
    if let lane { parts.append(#""lane":"\#(lane)""#) }
    return try decode("{" + parts.joined(separator: ",") + "}")
}

private func entryData(_ json: String) throws -> EntryData { try decode(json) }

// MARK: turns

@Test func turnsClampToOneThroughAHundred() {
    #expect(Turns.clamp(0) == 1 && Turns.clamp(-5) == 1)
    #expect(Turns.clamp(10) == 10 && Turns.clamp(100) == 100)
    #expect(Turns.clamp(101) == 100 && Turns.clamp(10_000) == 100)
    #expect(Turns.standard == 10 && Turns.picks == [1, 5, 10, 25, 100])
}

@Test func turnsReadAsWords() {
    #expect(Turns.words(1) == "1 turn" && Turns.words(10) == "10 turns")
}

// MARK: entries

@Test func aReplyFromBeforeCarriesNoneOfTheNewFields() throws {
    let data = try entryData(#"{"text":"hi","final":true,"author":"ada"}"#)
    #expect(data.conversation == nil && data.late == nil && data.lateWhy == nil && data.unprompted == nil)
    #expect(!data.isLate && data.lateReason == nil)
}

@Test func aLateReplyNamesWhy() throws {
    for (word, why) in [("stopped", LateWhy.stopped), ("newer", .newer), ("ended", .ended), ("noTurn", .noTurn)] {
        let data = try entryData(#"{"text":"x","conversation":"cv1","late":true,"lateWhy":"\#(word)"}"#)
        #expect(data.isLate && data.lateReason == why && data.conversation == "cv1")
        #expect(why.line.hasSuffix("so it starts nothing."))
    }
}

@Test func lateWithNoReasonOrAnUnknownOneIsStillLateButSaysNothingMore() throws {
    let bare = try entryData(#"{"text":"x","late":true}"#)
    let odd = try entryData(#"{"text":"x","late":true,"lateWhy":"somethingNew"}"#)
    #expect(bare.isLate && bare.lateReason == nil)
    #expect(odd.isLate && odd.lateReason == nil)
}

@Test func anUnpromptedPostIsNotLate() throws {
    let data = try entryData(#"{"text":"x","unprompted":true,"conversation":"cv1"}"#)
    #expect(!data.isLate && data.unprompted == true)
    // Even if a host said both, the room was simply quiet.
    #expect(try !entryData(#"{"text":"x","unprompted":true,"late":true}"#).isLate)
}

@Test func aBotPostIsAUserEntryWithASender() throws {
    let entry: Entry = try decode(#"""
    {"id":"e1","seq":1,"botId":"room","rev":1,"kind":"user","turn":1,"createdAt":0,"updatedAt":0,
     "data":{"text":"hi","sender":{"botId":"bg1","name":"Grok","provider":"grok"},"conversation":"cv1"}}
    """#)
    #expect(entry.isRoomBotPost && entry.data.roomBot?.provider == "grok")
    let mine: Entry = try decode(#"{"id":"e2","seq":2,"botId":"room","rev":2,"kind":"user","turn":1,"createdAt":0,"updatedAt":0,"data":{"text":"hi"}}"#)
    #expect(!mine.isRoomBotPost)
    // An older host sends no provider: the post still has its sender.
    let old: Entry = try decode(#"{"id":"e3","seq":3,"botId":"room","rev":3,"kind":"user","turn":1,"createdAt":0,"updatedAt":0,"data":{"text":"hi","sender":{"botId":"bg1","name":"Grok"}}}"#)
    #expect(old.isRoomBotPost && old.data.roomBot?.provider == nil)
}

@Test func theFourRoomNoticesDecode() throws {
    for (notice, text) in [("botJoined", "Grok joined · sees this room from now on"), ("botLeft", "Grok left"),
                           ("botPassed", "Grok passed"), ("botTimedOut", "Grok didn't answer")] {
        let entry: Entry = try decode("""
        {"id":"n","seq":1,"botId":"room","rev":1,"kind":"notice","turn":1,"createdAt":0,"updatedAt":0,
         "data":{"notice":"\(notice)","text":"\(text)","style":"info","bot":{"botId":"bg1","name":"Grok","provider":"grok"},"conversation":null,"why":"removed"}}
        """)
        #expect(entry.isRoomNotice && entry.data.text == text && entry.isChat)
    }
    let other: Entry = try decode(#"{"id":"n","seq":1,"botId":"room","rev":1,"kind":"notice","turn":1,"createdAt":0,"updatedAt":0,"data":{"text":"Routine","style":"info"}}"#)
    #expect(!other.isRoomNotice)
}

// MARK: history and events

@Test func historyHasTheConversationAbsentNullOrPresent() throws {
    let absent: HistoryPage = try decode(#"{"entries":[]}"#)
    let null: HistoryPage = try decode(#"{"entries":[],"conversation":null}"#)
    let present: HistoryPage = try decode(#"""
    {"entries":[],"conversation":{"conversation":"cv1","chat":"room","lane":null,"budget":10,"left":4,"state":"open","endedWhy":null,
     "current":{"id":"ada","kind":"agent","name":"Ada"},"waitUntil":null,"opening":"o1"}}
    """#)
    #expect(absent.conversation == nil && null.conversation == nil)
    #expect(present.conversation?.left == 4 && present.conversation?.current?.name == "Ada")
}

@Test func theConversationEventDecodesFlatAndToleratesNewWords() throws {
    let json = #"""
    {"type":"conversation","conversation":"cv1","chat":"room","lane":null,"budget":10,"left":6,"state":"open","endedWhy":null,
     "current":{"id":"bg1","kind":"bot","name":"Grok"},"waitUntil":1000,"opening":"o1"}
    """#
    guard case let .conversation(c)? = HostClient.parseEvent(Data(json.utf8)) else { Issue.record("not a conversation event"); return }
    #expect(c.isOpen && c.current?.kind == .bot && c.waitUntil == 1000 && c.lane == nil)
    // A state or a reason this version doesn't know reads as over, with its own word for the reason.
    let odd = try conversation(state: "paused", why: "weather")
    #expect(!odd.isOpen && odd.endedWhy == .unknown && odd.endLine == nil)
}

@Test func anOlderEventNeverUndoesANewerOne() throws {
    var book = RoomConversations()
    #expect(book.note(try conversation(left: 7)))
    #expect(book.note(try conversation(left: 6)))
    // A report from before the last charge arrives late.
    #expect(!book.note(try conversation(left: 7)))
    #expect(book.conversation("cv1")?.left == 6)
    // The end beats any open state, even one reporting fewer turns left.
    #expect(book.note(try conversation(state: "ended", why: "spent", left: 0)))
    #expect(!book.note(try conversation(state: "open", left: 3)))
    #expect(book.conversation("cv1")?.endedWhy == .spent)
    // An end that has a reason beats one that has none, never the other way.
    var ends = RoomConversations()
    #expect(ends.note(try conversation(state: "ended", left: 6)))
    #expect(ends.note(try conversation(state: "ended", why: "quiet", left: 6)))
    #expect(!ends.note(try conversation(state: "ended", left: 6)))
    #expect(ends.conversation("cv1")?.endedWhy == .quiet)
}

@Test func aNewMessageEndsTheOldRunAndStartsANewOne() throws {
    var book = RoomConversations()
    book.note(try conversation("cv1", left: 6))
    book.note(try conversation("cv1", state: "ended", why: "newer", left: 6))
    book.note(try conversation("cv2", left: 9))
    #expect(book.open(chat: "room", lane: nil)?.conversation == "cv2")
    #expect(book.latest(chat: "room", lane: nil)?.conversation == "cv2")
    #expect(book.ended(chat: "room", lane: nil).map(\.conversation) == ["cv1"])
    // A thread's run is its own lane.
    book.note(try conversation("cv3", lane: "t1"))
    #expect(book.open(chat: "room", lane: "t1")?.conversation == "cv3")
    #expect(book.open(chat: "room", lane: nil)?.conversation == "cv2")
}

@Test func theLanesLatestFromAHistoryAnswerIsTheNewestEvenIfHeardFirst() throws {
    var book = RoomConversations()
    book.note(try conversation("cv1", state: "ended", why: "newer"))
    book.note(try conversation("cv2"))
    book.note(try conversation("cv1", state: "ended", why: "newer"), newest: true)
    #expect(book.latest(chat: "room", lane: nil)?.conversation == "cv1")
}

// MARK: lines

@Test func theEndLinesSayHowTheRunEnded() throws {
    let spent = try #require(try conversation(state: "ended", why: "spent", left: 0).endLine)
    #expect(spent.text == "Used all 10 turns" && spent.offersMore)
    #expect(RoomEndLine.moreLabel == "Give them 10 more" && RoomEndLine.moreText == "Keep going" && RoomEndLine.moreTurns == 10)
    let quiet = try #require(try conversation(state: "ended", why: "quiet", left: 4).endLine)
    #expect(quiet.text == "Worked it out · 6 of 10 turns" && !quiet.offersMore)
    let stopped = try #require(try conversation(state: "stopped", why: "stopped", left: 6).endLine)
    #expect(stopped.text == "You stopped it · 4 of 10 turns used")
    // A Stop from a host that gave no reason still reads as one.
    #expect(try conversation(state: "stopped", left: 6).endLine?.text == "You stopped it · 4 of 10 turns used")
}

@Test func aQuietDayAndANewerMessageAndAnOldEndNeedNoLine() throws {
    #expect(try conversation(state: "ended", why: "idle").endLine == nil)
    #expect(try conversation(state: "ended", why: "newer").endLine == nil)
    // Ended before hosts kept the reason.
    #expect(try conversation(state: "ended").endLine == nil)
    #expect(try conversation(state: "open").endLine == nil)
}

@Test func theLiveLineShowsWhoseTurnAndTheTurnsLeft() throws {
    let agent = try #require(try conversation(left: 6, current: #"{"id":"coder","kind":"agent","name":"coder"}"#).liveLine())
    #expect(agent.title == "coder is answering" && agent.detail == "6 of 10 turns left" && agent.fraction == 0.4 && !agent.botTurn)
    let waiting = try conversation(left: 7, current: #"{"id":"bg1","kind":"bot","name":"Grok"}"#, waitUntil: 1_000_000 + 5 * 60_000)
    let bot = try #require(waiting.liveLine(now: Date(timeIntervalSince1970: 1000)))
    #expect(bot.title == "Grok · waits up to 5 min" && bot.botTurn)
    // Nobody up between turns, and no line at all once it is over.
    #expect(try conversation(left: 6).liveLine()?.title == "Working it out")
    #expect(try conversation(state: "ended", why: "spent", left: 0).liveLine() == nil)
}

@Test func theWaitCountsDownInMinutesAndNeverReadsZero() {
    let now = Date(timeIntervalSince1970: 1000)
    #expect(RoomLiveLine.wait(until: 1_000_000 + 4 * 60_000 + 1, now: now) == "waits up to 5 min")
    #expect(RoomLiveLine.wait(until: 1_000_000 + 20_000, now: now) == "waits up to 1 min")
    #expect(RoomLiveLine.wait(until: 999_000, now: now) == "waiting for it")
}

@Test func theBudgetIsNeverShownBeyondItself() throws {
    #expect(try conversation(state: "ended", why: "quiet", left: -2).used == 10)
    #expect(try conversation(state: "ended", why: "quiet", left: 14).used == 0)
}

// MARK: adding a bot

@Test func onlyTheOwnersActiveBotsCanJoinAndTheOthersSayWhy() {
    let mine = BotGuest(botId: "a", name: "Grok", state: .active)
    #expect(mine.roomBlock == nil)
    #expect(BotGuest(botId: "b", name: "Muse", state: .pending).roomBlock == "Waiting for you to let it in")
    #expect(BotGuest(botId: "c", name: "Nova", state: .invited).roomBlock == "Not connected yet")
    #expect(BotGuest(botId: "d", name: "Codex", state: .active, ownerPerson: "sam", ownerName: "Sam").roomBlock
            == "Sam's bot · only your own bots join your rooms")
    #expect(BotGuest.roomCandidates([mine, BotGuest(botId: "e", name: "Old", state: .revoked)]).map(\.botId) == ["a"])
}

@Test func howLongARoomWaitsForABot() {
    #expect(BotGuest(botId: "a", name: "Grok", state: .active).waitWords == "waits up to 5 min")
    let quick = BotGuest(botId: "a", name: "Grok", state: .active, limits: BotGuestLimits(ratePerHour: 1, concurrent: 1, turnTimeoutS: 30, wakeProfile: "p"))
    #expect(quick.waitWords == "waits up to 30 s" && quick.isWoken)
}
