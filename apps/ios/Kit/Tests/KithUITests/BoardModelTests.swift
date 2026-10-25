import Foundation
import Testing
@testable import KithKit
@testable import KithUI

// Noon UTC, read with a UTC calendar, so "today" does not depend on where the tests run.
private let now = Date(timeIntervalSince1970: 1_800_014_400)
private let utc: Calendar = {
    var calendar = Calendar(identifier: .gregorian)
    calendar.timeZone = TimeZone(identifier: "UTC")!
    return calendar
}()
private var nowMs: Int64 { Int64(now.timeIntervalSince1970 * 1000) }

private func bot(_ id: String, name: String? = nil, working: [String]? = nil, activity: String = "", folder: String? = nil,
                 lastAt: Int64 = 0, status: String = "idle") throws -> Bot {
    let laneList = (working ?? []).map { "{\"chat\":\"\(id)\",\"thread\":\"\($0)\"}" }.joined(separator: ",")
    let lanes = ",\"workingLanes\":[\(laneList)]"
    let place = folder.map { #","folder":{"kind":"project","project":"\#($0)","name":"\#($0)"}"# } ?? ""
    let json = #"{"id":"\#(id)","name":"\#(name ?? id)","kind":"agent","rev":1,"status":"\#(status)","activity":"\#(activity)","lastAt":\#(lastAt)\#(lanes)\#(place)}"#
    return try JSONDecoder().decode(Bot.self, from: Data(json.utf8))
}

private func entry(_ id: String, _ kind: String, chat: String, thread: String? = nil, at: Int64? = nil,
                   _ configure: (inout EntryData) -> Void = { _ in }) -> Entry {
    var data = EntryData()
    configure(&data)
    let created = at ?? nowMs
    return Entry(id: id, seq: created, botId: chat, threadId: thread, rev: 1, kind: kind, turn: 1, data: data, createdAt: created, updatedAt: created)
}

private func permission(_ id: String, chat: String, thread: String? = nil, at: Int64? = nil, status: String = "pending",
                        approver: String? = nil) -> Entry {
    entry(id, "permission", chat: chat, thread: thread, at: at) {
        $0.status = status
        $0.title = "swift test"
        $0.toolKind = "execute"
        $0.approver = approver
        $0.options = [PermissionOption(optionId: "no", name: "No", kind: "reject_once"),
                      PermissionOption(optionId: "yes", name: "Yes", kind: "allow_once")]
    }
}

private func task(_ id: String, chat: String, text: String, at: Int64? = nil) -> Entry {
    entry(id, "user", chat: chat, at: at) { $0.text = text; $0.task = true }
}

private func computer(_ id: String = "mac", trouble: BoardTrouble? = nil, bots: [Bot], entries: [String: [Entry]],
                      guests: [GuestPrompt] = []) -> BoardComputer {
    BoardComputer(id: id, hostName: "Mac \(id)", trouble: trouble, bots: bots, entries: entries, guestPrompts: guests)
}

private func board(_ computers: BoardComputer...) -> BoardModel { BoardModel(computers: computers, now: now, calendar: utc) }

@Test func questionsWaitingOnYouAreCountedAcrossComputersOldestFirst() throws {
    let a = computer("a", bots: [try bot("x")], entries: ["x": [permission("p2", chat: "x", at: nowMs - 1000), permission("p3", chat: "x", status: "answered")]])
    let guest = GuestPrompt(permission("g1", chat: "guest-chat", at: nowMs - 5000, approver: "owner"))
    let b = computer("b", bots: [try bot("y")], entries: ["y": [permission("p1", chat: "y", at: nowMs - 9000)]], guests: [guest])
    let model = board(a, b)
    #expect(model.waiting == 3)
    #expect(model.needsYou.map(\.entry?.id) == ["p1", "g1", "p2"])
    #expect(model.needsYou.first { $0.entry?.id == "g1" }?.target.botId == "guest-chat")
}

@Test func aRowCountsOnlyPendingCardsWaitingOnThisAccount() {
    let entries = [permission("a", chat: "x"), permission("b", chat: "x", approver: "member"), permission("c", chat: "x", approver: "owner"),
                   permission("d", chat: "x", status: "answered"), task("t", chat: "x", text: "go")]
    #expect(BoardModel.waitingCount(in: entries) == 2)
    #expect(BoardModel.waitingCount(in: []) == 0)
}

@Test func anOwnersCardInAMembersChatIsNotTheMembersToAnswer() throws {
    let member = computer(bots: [try bot("x")], entries: ["x": [permission("o", chat: "x", approver: "owner"), permission("m", chat: "x", approver: "member")]])
    #expect(board(member).needsYou.map(\.entry?.id) == ["m"])
}

@Test func aDecisionCardOffersOnlyTheCardsOwnAnswersAndNamesTheTask() throws {
    let entries = ["x": [task("t", chat: "x", text: "Fix the  sleep notice"), permission("p", chat: "x", thread: "t")]]
    let model = board(computer(bots: [try bot("x", name: "coder")], entries: entries))
    let card = try #require(model.needsYou.first).card
    #expect(card.heading == "coder · Fix the sleep notice")
    #expect(card.question == "Wants to run a command")
    #expect(card.detail == "swift test")
    #expect(card.options.map(\.id) == ["yes", "no"])
    #expect(card.options.first?.role == .primary)
}

@Test func workingTasksListTheTaskNotTheBotAndAWaitingTaskLeavesWorking() throws {
    let entries = ["x": [task("t1", chat: "x", text: "Relay fix"), task("t2", chat: "x", text: "Asks first"),
                         permission("p", chat: "x", thread: "t2")]]
    let model = board(computer(bots: [try bot("x", working: ["t1", "t2"], activity: "running tests", status: "working")], entries: entries))
    #expect(model.working.map(\.title) == ["Relay fix"])
    #expect(model.working.first?.detail == "running tests")
    #expect(model.needsYou.count == 1)
}

@Test func aSessionWorkingInATerminalIsOnTheBoardThoughNoHostLaneRunsIt() throws {
    let json = #"{"count":4,"lastAt":1,"authors":[],"liveWriter":"terminal","liveBusy":true}"#
    let live = task("t1", chat: "x", text: "Review the diff", at: nowMs - 3 * 86_400_000)
    var busy = live
    busy.data.thread = try JSONDecoder().decode(ThreadSummary.self, from: Data(json.utf8))
    let model = board(computer(bots: [try bot("x")], entries: ["x": [busy]]))
    #expect(model.working.map(\.title) == ["Review the diff"])
}

@Test func aBotAtWorkWithoutATaskIsOneRow() throws {
    let model = board(computer(bots: [try bot("x", name: "docs", working: [], activity: "writing")], entries: [:]))
    // Not working in its main chat (no lane): nothing. With a main-chat lane: one row by bot name.
    #expect(model.working.isEmpty)
    let json = #"{"id":"g","name":"docs","kind":"agent","rev":1,"status":"working","activity":"writing","workingLanes":[{"chat":"g"}]}"#
    let busy = try JSONDecoder().decode(Bot.self, from: Data(json.utf8))
    #expect(board(computer(bots: [busy], entries: [:])).working.map(\.title) == ["docs"])
}

@Test func doneTodayHoldsOnlyFinishedTasksOfTodayNewestFirstAndCapped() throws {
    var list: [Entry] = []
    for i in 0..<7 { list.append(task("t\(i)", chat: "x", text: "Task \(i)", at: nowMs - Int64(i) * 60_000)) }
    list.append(task("old", chat: "x", text: "Yesterday", at: nowMs - 3 * 86_400_000))
    let model = board(computer(bots: [try bot("x")], entries: ["x": list]))
    #expect(model.doneToday.count == BoardModel.doneLimit)
    #expect(model.doneToday.first?.title == "Task 0")
    #expect(!model.doneToday.contains { $0.title == "Yesterday" })
}

@Test func aStoppedTaskIsNotDone() throws {
    let list = [task("t", chat: "x", text: "Stopped one"), entry("n", "notice", chat: "x", thread: "t") { $0.text = "Stopped." }]
    #expect(board(computer(bots: [try bot("x")], entries: ["x": list])).doneToday.isEmpty)
}

@Test func anEmptyBoardIsEmptyAndShowsNoBadge() throws {
    let model = board(computer(bots: [try bot("x")], entries: [:]))
    #expect(model.isEmpty)
    #expect(model.waiting == 0)
    #expect(CountBadge.text(for: model.waiting) == nil)
    #expect(BoardModel().isEmpty)
}

@Test func theBadgeCapsAtNinetyNinePlus() throws {
    let entries = (0..<120).map { permission("p\($0)", chat: "x", at: Int64($0)) }
    let model = board(computer(bots: [try bot("x")], entries: ["x": entries]))
    #expect(model.waiting == 120)
    #expect(CountBadge.text(for: model.waiting) == "99+")
}

@Test func anAnsweredQuestionLeavesTheBoard() throws {
    let open = ["x": [permission("p", chat: "x")]]
    #expect(board(computer(bots: [try bot("x")], entries: open)).waiting == 1)
    #expect(board(computer(bots: [try bot("x")], entries: ["x": [permission("p", chat: "x", status: "answered")]])).waiting == 0)
}

@Test func aHandoffShowsOnceWhileFreshAndNotWhenKeptOrStale() throws {
    func handoff(_ id: String, status: String, age: TimeInterval, text: String = "We fixed the hook.") -> Entry {
        entry(id, "notice", chat: "x", at: nowMs - Int64(age * 1000)) {
            $0.handoff = HandoffCard(status: status, reason: "cold", text: text)
        }
    }
    let fresh = board(computer(bots: [try bot("x")], entries: ["x": [handoff("old", status: "sent", age: 3600), handoff("new", status: "draft", age: 60)]]))
    #expect(fresh.leftOff.map(\.id) == ["mac/new"])
    #expect(fresh.leftOff.first?.card.note == "We fixed the hook.")
    #expect(board(computer(bots: [try bot("x")], entries: ["x": [handoff("k", status: "kept", age: 60)]])).leftOff.isEmpty)
    #expect(board(computer(bots: [try bot("x")], entries: ["x": [handoff("s", status: "sent", age: BoardModel.handoffAge + 60)]])).leftOff.isEmpty)
    #expect(board(computer(bots: [try bot("x")], entries: ["x": [handoff("e", status: "sent", age: 60, text: "  ")]])).leftOff.isEmpty)
    let long = board(computer(bots: [try bot("x")], entries: ["x": [handoff("l", status: "sent", age: 60, text: String(repeating: "word ", count: 200))]]))
    #expect((long.leftOff.first?.card.note?.count ?? 0) <= BoardModel.noteLimit + 1)
}

@Test func anUnreachableComputerKeepsItsItemsWithATroubleFlagAndAWait() throws {
    let model = board(computer("m", trouble: .unreachable, bots: [try bot("x")], entries: ["x": [permission("p", chat: "x")]]))
    #expect(model.needsYou.first?.trouble == .unreachable)
    #expect(model.waits.first?.kind == .computer(.unreachable))
    #expect(model.waits.first?.target == nil)
}

@Test func queuedMessagesAreWaitsOnTheirBot() throws {
    let queued = entry("q", "user", chat: "x") { $0.status = "queued"; $0.text = "later" }
    let model = board(computer(bots: [try bot("x", name: "coder")], entries: ["x": [queued, queued]]))
    #expect(model.waits.map(\.kind) == [.queued(2)])
    #expect(model.waits.first?.title == "coder")
}

@Test func projectsGroupAgentsByPlaceAndSkipBotsWithoutOne() throws {
    let model = board(computer(bots: [try bot("a", folder: "kith", lastAt: 1), try bot("b", folder: "kith", lastAt: 5, status: "working"),
                                      try bot("c")], entries: [:]))
    #expect(model.projects.count == 1)
    #expect(model.projects.first?.agents == 2)
    #expect(model.projects.first?.working == 1)
    #expect(model.projects.first?.target.botId == "b")
}

@Test func theDockFiltersBoardItemsInPlaceAndReturnOpensTheTopMatch() throws {
    let entries = ["x": [task("t1", chat: "x", text: "Relay fix"), task("t2", chat: "x", text: "Night sky crops"), permission("p", chat: "x")]]
    let model = board(computer(bots: [try bot("x", name: "coder", working: ["t1", "t2"], status: "working")], entries: entries))
    #expect(model.working.count == 2)
    let relay = model.filtered(by: "relay")
    #expect(relay.working.map(\.title) == ["Relay fix"])
    #expect(relay.needsYou.isEmpty)
    #expect(relay.top?.botId == "x")
    let run = model.filtered(by: "run")
    #expect(run.needsYou.map(\.entry?.id) == ["p"])
    #expect(run.working.isEmpty)
    #expect(model.filtered(by: "zzz").isEmpty)
    #expect(model.filtered(by: "zzz").top == nil)
    #expect(model.filtered(by: "  ") == model)
}

// MARK: - Access and Key cards

private func keyCard(_ id: String, chat: String) -> Entry {
    entry(id, "permission", chat: chat) {
        $0.status = "pending"
        $0.card = "key"
        $0.secret = SecretRequest(service: "github", vars: ["GITHUB_TOKEN"])
    }
}

@Test func aKeyCardIsARowThatOpensTheChatNeverAnAnswer() throws {
    let model = board(computer(bots: [try bot("x", name: "Ada")], entries: ["x": [keyCard("k1", chat: "x")]]))
    let row = try #require(model.needsYou.first)
    #expect(row.opensChat)
    #expect(row.card.question == "Ada needs a key for github")
    #expect(row.card.options.isEmpty)
    #expect(row.target.botId == "x")
    #expect(model.waiting == 1)
}

@Test func anOrdinaryCardStillAnswersOnTheBoard() throws {
    let model = board(computer(bots: [try bot("x")], entries: ["x": [permission("p1", chat: "x")]]))
    #expect(model.needsYou.first?.opensChat == false)
    #expect(model.needsYou.first?.card.options.count == 2)
}

@Test func aKeyCardIsNeverAGuestPrompt() {
    var key = keyCard("k1", chat: "guest-chat")
    key.data.approver = "owner"
    let plain = permission("p1", chat: "guest-chat", approver: "owner")
    let prompts = GuestPrompt.pending(in: ["guest-chat": [key, plain]], known: [])
    #expect(prompts.map(\.id) == ["p1"])
}
