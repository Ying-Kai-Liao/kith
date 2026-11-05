import Foundation
import Testing
@testable import KithKit

private func entry(_ id: String, _ kind: String, thread: String? = nil, _ configure: (inout EntryData) -> Void = { _ in }) -> Entry {
    var data = EntryData()
    configure(&data)
    return Entry(id: id, seq: 0, botId: "b", threadId: thread, rev: 1, kind: kind, turn: 1, data: data, createdAt: 0, updatedAt: 0)
}

private func bot(_ json: String) throws -> Bot {
    try JSONDecoder().decode(Bot.self, from: Data(json.utf8))
}

@Test func theNewestTaskIsTheCurrentOne() {
    let a = entry("a", "user") { $0.text = "fix the build"; $0.task = true }
    let b = entry("b", "user") { $0.text = "and the docs"; $0.task = true }
    #expect(Tasks.current(in: [a, b, entry("n", "notice")])?.id == "b")
    #expect(Tasks.current(in: [entry("u", "user") { $0.text = "before tasks" }]) == nil)
    #expect(Tasks.current(in: [entry("r", "user", thread: "a") { $0.task = true }]) == nil)
}

@Test func aFinishedTaskTakesTheHostsStatusAndAnOlderHostFallsBack() throws {
    let idle = try bot(#"{"id":"b","workingLanes":[]}"#)
    func root(_ status: String?) throws -> Entry {
        var e = entry("t", "user") { $0.task = true }
        let field = status.map { #","status":"\#($0)""# } ?? ""
        e.data.thread = try JSONDecoder().decode(ThreadSummary.self, from: Data(#"{"count":1,"lastAt":1,"authors":[]\#(field)}"#.utf8))
        return e
    }
    let failedNotice = entry("f", "notice", thread: "t") { $0.text = "The agent failed: boom" }
    #expect(Tasks.status(of: try root("failed"), bot: idle, replies: []) == .failed)
    #expect(Tasks.status(of: try root("stopped"), bot: idle, replies: []) == .stopped)
    #expect(Tasks.status(of: try root("done"), bot: idle, replies: [failedNotice]) == .done)
    #expect(Tasks.status(of: try root(nil), bot: idle, replies: [failedNotice]) == .done)
    #expect(Tasks.status(of: try root("working"), bot: idle, replies: []) == .done)
    #expect(TaskStatus.failed.label == "Failed")
}

@Test func aTaskIsWorkingWaitingStoppedOrDone() throws {
    let root = entry("t", "user") { $0.task = true }
    let working = try bot(#"{"id":"b","workingLanes":[{"chat":"b","thread":"t"}]}"#)
    let idle = try bot(#"{"id":"b","workingLanes":[]}"#)
    let stopped = entry("s", "notice", thread: "t") { $0.text = "Stopped." }
    #expect(Tasks.status(of: root, bot: working, replies: []) == .working)
    #expect(Tasks.status(of: root, bot: working, replies: [entry("p", "permission", thread: "t") { $0.status = "pending" }]) == .needsYou)
    #expect(Tasks.status(of: root, bot: idle, replies: [stopped]) == .stopped)
    #expect(Tasks.status(of: root, bot: idle, replies: [stopped, entry("r", "agent", thread: "t") { $0.final = true }]) == .done)
}

@Test func aCardOpensWhileItWorksAndFoldsWhenDoneUnlessTheUserChose() {
    #expect(Tasks.isOpen(.working))
    #expect(!Tasks.isOpen(.done))
    #expect(!Tasks.isOpen(.needsYou))
    #expect(!Tasks.isOpen(.stopped))
    #expect(!Tasks.isOpen(.working, choice: false))
    #expect(Tasks.isOpen(.done, choice: true))
}

@Test func aVoiceCallFollowsTheChatAndItsTasksNotOtherThreads() {
    let all = [entry("p", "agent"), entry("t", "user") { $0.task = true }, entry("a", "agent", thread: "t"),
               entry("x", "agent", thread: "p")]
    #expect(Tasks.chatAndTasks(all).map(\.id) == ["p", "t", "a"])
}

@Test func aCardIsTitledByItsFirstMessage() throws {
    #expect(Tasks.title(entry("t", "user") { $0.text = "  fix\nthe   build "; $0.task = true }) == "fix the build")
    #expect(Tasks.title(entry("t", "user") { $0.task = true }) == "Task")
    #expect(try bot(#"{"id":"b"}"#).usesTasks)
    #expect(try !bot(#"{"id":"b","tasks":false}"#).usesTasks)
    #expect(try !bot(#"{"id":"g","kind":"group"}"#).usesTasks)
}

@Test func aTaskStillTalkingAfterTheChatsLastMessageMovesToTheEnd() throws {
    func task(_ id: String, at: Int64, lastAt: Int64) throws -> Entry {
        var e = entry(id, "user") { $0.task = true }
        e.createdAt = at
        e.data.thread = try JSONDecoder().decode(ThreadSummary.self, from: Data(#"{"count":3,"lastAt":\#(lastAt),"authors":[]}"#.utf8))
        return e
    }
    var note = entry("n", "notice") { $0.text = "Voice chat" }
    note.createdAt = 300
    let old = try task("old", at: 10, lastAt: 500)
    let quiet = try task("quiet", at: 20, lastAt: 25)
    let placed = Tasks.place([old, quiet, note].map(BotExchangeGroup.Item.entry))
    #expect(placed.slots.map(\.id) == ["quiet", "n", "old"])
    #expect(placed.moved == ["old"])
    // A chat whose only row is the task root, or one with later messages, keeps its order.
    #expect(Tasks.place([.entry(old)]).slots.map(\.id) == ["old"])
    var later = entry("m", "user") { $0.text = "hi" }
    later.createdAt = 900
    let settled = Tasks.place([old, later].map(BotExchangeGroup.Item.entry))
    #expect(settled.slots.map(\.id) == ["old", "m"])
    #expect(settled.moved.isEmpty)
}

private func summary(_ json: String) throws -> ThreadSummary {
    try JSONDecoder().decode(ThreadSummary.self, from: Data(json.utf8))
}

@Test func theFoldShowsTheLastReplyAndCountsOnlyWhatItHides() {
    let thread = [
        entry("a1", "agent", thread: "t") { $0.final = true },
        entry("s1", "tool", thread: "t"),
        entry("s2", "tool", thread: "t"),
        entry("u2", "user", thread: "t"),
        entry("think", "agent", thread: "t"),
        entry("p", "permission", thread: "t") { $0.status = "pending" },
        entry("a2", "agent", thread: "t") { $0.final = true },
    ]
    let fold = Tasks.fold(thread) { _ in true }
    #expect(fold.last?.id == "a2")
    #expect(fold.messages == 2 && fold.steps == 2)
    #expect(Tasks.foldLine(messages: 2, steps: 2) == "2 messages · 2 steps")
    #expect(Tasks.foldLine(messages: 0, steps: 1) == "1 step")
    #expect(Tasks.foldLine(messages: 1, steps: 0) == "1 message")
    #expect(Tasks.foldLine(messages: 0, steps: 0) == nil)
    // A tool that delivered a file is not a step.
    #expect(Tasks.fold(thread) { $0.id != "s1" }.steps == 1)
}

@Test func theHeadShowsStatusOriginOrTheTerminalHoldingIt() throws {
    #expect(Tasks.head(.done, thread: nil, mac: "Mac mini") == TaskHead(status: nil, origin: nil, held: false))
    let telegram = try summary(#"{"count":1,"lastAt":1,"authors":[],"origin":"telegram"}"#)
    #expect(Tasks.head(.done, thread: telegram, mac: "Mac mini") == TaskHead(status: nil, origin: "Telegram", held: false))
    let held = try summary(#"{"count":1,"lastAt":1,"authors":[],"liveWriter":"terminal","liveBusy":true}"#)
    #expect(Tasks.head(.working, thread: held, mac: "Mac mini") == TaskHead(status: "Working", origin: "In terminal · Mac mini", held: true))
    #expect(Tasks.head(.needsYou, thread: nil, mac: "") .status == "Needs you")
}

@Test func theBoxGoesOnWithTheNewestTaskWhileItWorksOrForTenIdleMinutes() throws {
    let now: Int64 = 100 * Tasks.idleMs
    let idle = try bot(#"{"id":"b","workingLanes":[]}"#)
    func task(_ id: String, at: Int64, origin: String? = nil) throws -> Entry {
        var e = entry(id, "user") { $0.task = true; $0.text = id }
        e.createdAt = at
        if let origin { e.data.thread = try summary(#"{"count":0,"lastAt":0,"authors":[],"origin":"\#(origin)"}"#) }
        return e
    }
    let fresh = try task("a", at: now - 60_000)
    #expect(Tasks.continuing(bot: idle, chat: [fresh], now: now)?.id == "a")
    #expect(Tasks.continuing(bot: idle, chat: [try task("a", at: now - Tasks.idleMs)], now: now) == nil)
    let working = try bot(#"{"id":"b","workingLanes":[{"chat":"b","thread":"old"}]}"#)
    #expect(Tasks.continuing(bot: working, chat: [try task("old", at: 0)], now: now)?.id == "old")
    // A newer terminal session isn't gone on with by itself; the app's task before it is.
    #expect(Tasks.continuing(bot: idle, chat: [fresh, try task("term", at: now - 1_000, origin: "terminal")], now: now)?.id == "a")

    #expect(Tasks.of(entry("r", "agent", thread: "a")) == "a")
    #expect(Tasks.of(fresh) == "a")
    #expect(Tasks.of(entry("n", "user") { $0.text = "plain" }) == nil)
}

@Test func theBoxSendsWhereThePillsSay() throws {
    let now: Int64 = 100 * Tasks.idleMs
    let idle = try bot(#"{"id":"b","workingLanes":[]}"#)
    var old = entry("old", "user") { $0.task = true }
    old.createdAt = 0
    var fresh = entry("fresh", "user") { $0.task = true }
    fresh.createdAt = now - 60_000
    let chat = [old, fresh]
    let unset = Tasks.destination(target: nil, bot: idle, chat: chat, now: now)
    #expect(unset.going?.id == "fresh" && unset.into?.id == "fresh" && !unset.picked && !unset.newTask)
    let picked = Tasks.destination(target: .task("old"), bot: idle, chat: chat, now: now)
    #expect(picked.going?.id == "old" && picked.into?.id == "old" && picked.picked)
    // New task: nothing carried, but the Continues pill still names a task to switch back to.
    let new = Tasks.destination(target: .new, bot: idle, chat: chat, now: now)
    #expect(new.into == nil && new.newTask && new.going?.id == "fresh")
    // A picked task that is gone falls back to the one the host would go on with.
    #expect(Tasks.destination(target: .task("gone"), bot: idle, chat: chat, now: now).going?.id == "fresh")
}

private struct TerminalBusy: Error {}

@Test @MainActor func aSendToAPickedTaskAnIdleTerminalHoldsGoesOnlyAfterTheTakeOver() async throws {
    let held = try summary(#"{"count":1,"lastAt":1,"authors":[],"origin":"terminal","liveWriter":"terminal","liveBusy":false}"#)
    var calls: [String] = []
    #expect(Tasks.takesOver(held, picked: true))
    let ok = await Tasks.sendIn(held, picked: true, takeOver: { calls.append("take") }, send: { calls.append("send") })
    #expect(ok == nil)
    #expect(calls == ["take", "send"])
}

@Test @MainActor func aFailedTakeOverSendsNothing() async throws {
    let held = try summary(#"{"count":1,"lastAt":1,"authors":[],"origin":"terminal","liveWriter":"terminal","liveBusy":false}"#)
    var calls: [String] = []
    let failed = await Tasks.sendIn(held, picked: true, takeOver: { calls.append("take"); throw TerminalBusy() },
                                    send: { calls.append("send") })
    #expect(failed is TerminalBusy, "the caller keeps the text in the box and shows why")
    #expect(calls == ["take"], "never sent, so never a new task")
}

@Test @MainActor func aBusyTerminalOrAnUnpickedTaskIsSentToAsTheHostRoutesIt() async throws {
    let busy = try summary(#"{"count":1,"lastAt":1,"authors":[],"liveWriter":"terminal","liveBusy":true}"#)
    let idleHeld = try summary(#"{"count":1,"lastAt":1,"authors":[],"liveWriter":"terminal","liveBusy":false}"#)
    var calls: [String] = []
    _ = await Tasks.sendIn(busy, picked: true, takeOver: { calls.append("take") }, send: { calls.append("send") })
    #expect(calls == ["send"])
    calls = []
    _ = await Tasks.sendIn(nil, picked: true, takeOver: { calls.append("take") }, send: { calls.append("send") })
    #expect(calls == ["send"])
    calls = []
    #expect(!Tasks.takesOver(idleHeld, picked: false))
    _ = await Tasks.sendIn(idleHeld, picked: false, takeOver: { calls.append("take") }, send: { calls.append("send") })
    #expect(calls == ["send"])
}
