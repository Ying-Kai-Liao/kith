import Foundation
import Testing
@testable import KithKit
@testable import KithUI

private func entry(_ seq: Int64, _ kind: String, configure: (inout EntryData) -> Void = { _ in }) -> Entry {
    var data = EntryData()
    configure(&data)
    return Entry(id: "e\(seq)", seq: seq, botId: "b", threadId: "r", rev: seq, kind: kind, turn: 1, data: data, createdAt: 0, updatedAt: 0)
}

private func tool(_ seq: Int64) -> Entry {
    entry(seq, "tool") { $0.toolKind = "execute"; $0.title = "swift test"; $0.status = "completed" }
}

@Test func openThreadShowsRepliesAndToolRowsInOrderAndHidesThinking() {
    let replies = [entry(1, "thought") { $0.text = "hm" }, tool(2), entry(3, "agent") { $0.text = "Done."; $0.final = true },
                   entry(4, "plan")]
    let out = ThreadFeed.replies(replies)
    #expect(out.map(\.id) == ["steps-e2", "e3"])
}

@Test func aRunOfToolCallsIsOneStepsReply() {
    let out = ThreadFeed.replies((1...7).map(tool))
    #expect(out.count == 1 && out.first?.id == "steps-e1")
    guard case let .steps(run) = out.first else { Issue.record("no steps"); return }
    #expect(run.steps.count == 7 && run.hidden == 0)
}

@Test func aVeryLongRunKeepsTheNewestTwentyAndCountsTheRestAsEarlier() {
    guard case let .steps(run) = ThreadFeed.replies((1...25).map(tool)).first else { Issue.record("no steps"); return }
    #expect(run.hidden == 5 && run.shown.count == ThreadFeed.visibleSteps && run.shown.first?.id == "e6")
}

@Test func deliveryToolsDropOutOfARun() {
    let send = entry(1, "tool") { $0.title = "Used mcp__kith-files__send_file"; $0.status = "completed" }
    #expect(ThreadFeed.replies([send]).isEmpty)
    // Other kith-files tools deliver nothing: they stay.
    let ask = entry(2, "tool") { $0.title = "Used mcp__kith-files__request_secret"; $0.status = "completed" }
    #expect(ThreadFeed.replies([send, ask]).map(\.id) == ["steps-e1"])
}

@Test func mcpToolsReadAsAHumanVerbWithoutTheRawName() {
    let row = ThreadFeed.toolRow(entry(1, "tool") { $0.title = "Used mcp__kith-files__send_file"; $0.status = "completed" })
    #expect(row.verb == "Send file" && !row.object.contains("mcp__") && !row.verb.contains("mcp__"))
    let hyphen = ThreadFeed.toolRow(entry(2, "tool") { $0.title = "mcp__docs__list-pages x"; $0.command = "mcp__docs__list-pages" })
    #expect(hyphen.verb == "List pages" && hyphen.object == "x")
    // Named only in the command: the title stays as the object, still without the raw name.
    let viaCommand = ThreadFeed.toolRow(entry(3, "tool") { $0.title = "Fetch page"; $0.command = "mcp__docs__fetch_page" })
    #expect(viaCommand.verb == "Fetch page")
}

@Test func toolRowReadsVerbObjectAndResult() {
    let edit = entry(1, "tool") {
        $0.toolKind = "edit"; $0.status = "completed"
        $0.diffs = [FileDiff(path: "/a/kith-sessions.py", added: 6, removed: 2, isNew: false, startLine: 1, patch: "+x")]
    }
    let row = ThreadFeed.toolRow(edit)
    #expect(row.verb == "Edited" && row.object == "kith-sessions.py" && row.result == .text("+6 -2"))
    #expect(row.output == "+x")

    let failed = ThreadFeed.toolRow(entry(2, "tool") { $0.toolKind = "execute"; $0.title = "cargo build"; $0.status = "failed" })
    #expect(failed.result == .error("failed") && failed.command == "cargo build")

    // Still running: no result, so the row shows its spinner; unknown kinds read "Used".
    let running = ThreadFeed.toolRow(entry(3, "tool") { $0.title = "mcp thing"; $0.status = "in_progress" })
    #expect(running.verb == "Used" && running.result == nil)
}

@Test func aTitleThatOpensWithTheVerbIsNotDoubled() {
    func object(_ kind: String, _ title: String) -> String {
        ThreadFeed.toolRow(entry(1, "tool") { $0.toolKind = kind; $0.title = title }).object
    }
    #expect(object("read", "Read src/pace.js") == "src/pace.js")
    #expect(object("read", "reading src/pace.js") == "src/pace.js")
    #expect(object("edit", "Write src/pace.js") == "src/pace.js")
    #expect(object("search", "Searched for foo") == "for foo")
    #expect(object("fetch", "Fetch https://x.dev") == "https://x.dev")
    #expect(object("read", "src/pace.js") == "src/pace.js")
    #expect(object("read", "Read") == "Read")
    #expect(object("read", "Ran tests") == "Ran tests")
    #expect(object("execute", "Run npm test") == "Run npm test")
}

@Test func aPendingPermissionIsYourTurn() {
    let pending = entry(1, "permission") { $0.status = "pending" }
    let answered = entry(2, "permission") { $0.status = "answered" }
    #expect(ThreadFeed.hasQuestion([pending]))
    #expect(!ThreadFeed.hasQuestion([answered, entry(3, "agent")]))
}

@Test func placeMapsProjectsAndFoldersAndToleratesNone() {
    #expect(ThreadFeed.place(nil) == nil)
    #expect(ThreadFeed.place(PlaceFolder(kind: "project", project: "kith", name: "kith")) == .project("kith"))
    #expect(ThreadFeed.place(PlaceFolder(kind: "folder", path: "/x", name: "x")) == .folder("x"))
}

@Test func ownMessageKeepsItsNonceIdAndGapsGetSeparators() {
    let mine = entry(1, "user") { $0.clientNonce = "n1"; $0.text = "hi" }
    var later = entry(2, "agent") { $0.text = "ok"; $0.final = true }
    later.createdAt = 2 * 3_600_000
    let out = ThreadFeed.replies([mine, later], separators: true)
    #expect(out.map(\.id) == ["sep-user-n1", "user-n1", "sep-e2", "e2"])
}

private func summary(origin: String? = nil, writer: String? = nil, busy: Bool? = nil) -> ThreadSummary {
    ThreadSummary(count: 2, lastAt: 0, authors: ["b"], unread: nil, origin: origin, liveWriter: writer, liveBusy: busy)
}

@Test func heldThreadsMapToBannerPhases() {
    #expect(ThreadFeed.writerPhase(nil, taking: false, failed: false, unreachable: false) == nil)
    #expect(ThreadFeed.writerPhase(summary(), taking: false, failed: false, unreachable: false) == nil)
    #expect(ThreadFeed.writerPhase(summary(writer: "app"), taking: false, failed: false, unreachable: false) == nil)
    let held = summary(origin: "terminal", writer: "terminal", busy: false)
    #expect(ThreadFeed.writerPhase(held, taking: false, failed: false, unreachable: false) == .idle)
    #expect(ThreadFeed.writerPhase(summary(writer: "terminal", busy: true), taking: false, failed: false, unreachable: false) == .working)
    #expect(ThreadFeed.writerPhase(held, taking: true, failed: false, unreachable: false) == .taking)
    #expect(ThreadFeed.writerPhase(held, taking: false, failed: true, unreachable: false) == .failed)
    #expect(ThreadFeed.writerPhase(held, taking: false, failed: false, unreachable: true) == .unreachable)
}

@Test func aHeldThreadKeepsTheBannerAndTheComposerWhoseMessagesWait() {
    let root = entry(1, "user")
    #expect(ThreadFeed.chatBottom(held: root, mismatch: false, guest: false) == .writerBannerAndComposer(rootId: "e1"))
    #expect(ThreadFeed.chatBottom(held: nil, mismatch: false, guest: false) == .composer)
    // Read-only and update-needed stay as they were.
    #expect(ThreadFeed.chatBottom(held: root, mismatch: false, guest: true) == .writerBanner(rootId: "e1"))
    #expect(ThreadFeed.chatBottom(held: nil, mismatch: false, guest: true) == .readOnly)
    #expect(ThreadFeed.chatBottom(held: root, mismatch: true, guest: false) == .updateNeeded)
}

@Test func originAndWriterChipFollowTheSummary() {
    #expect(ThreadFeed.origin(summary(origin: "terminal")) == .terminal)
    #expect(ThreadFeed.origin(summary(origin: "slack")) == .slack)
    #expect(ThreadFeed.origin(summary(origin: "carrier-pigeon")) == nil)
    #expect(ThreadFeed.origin(summary()) == nil)
    #expect(ThreadFeed.writerChip(summary(writer: "terminal", busy: true), mac: "Mac mini") == .writer(working: true, mac: "Mac mini"))
    #expect(ThreadFeed.writerChip(summary(), mac: "Mac mini") == nil)
}

@Test func writerChangeNoticesBecomeDividers() {
    let notice = entry(1, "notice") { $0.writerChange = "tookOver" }
    #expect(ThreadFeed.writerDivider(notice, mac: "Mac mini") == .tookOver)
    #expect(ThreadFeed.writerDivider(entry(2, "notice") { $0.writerChange = "startedInTerminal" }, mac: "Mac mini") == .startedInTerminal(mac: "Mac mini"))
    #expect(ThreadFeed.writerDivider(entry(3, "notice") { $0.writerChange = "somethingNew" }, mac: "m") == nil)
    #expect(ThreadFeed.writerDivider(entry(4, "notice"), mac: "m") == nil)
}

@Test func aQuestionInAHeldThreadIsNotYoursToAnswer() {
    let pending = entry(1, "permission") { $0.status = "pending" }
    #expect(ThreadFeed.hasQuestion([pending]))
    #expect(!ThreadFeed.hasQuestion([pending], held: true))
}

@Test func toolRowsDoNotHideAMissingReply() {
    let summary = ThreadSummary(count: 3, lastAt: 0, authors: [])
    // Two chat entries loaded plus three tool rows: five entries, but one reply is still missing.
    let loaded = [entry(1, "user"), tool(2), tool(3), tool(4), entry(5, "agent") { $0.final = true }]
    #expect(ThreadFeed.isMissingReplies(loaded, summary: summary))
    #expect(!ThreadFeed.isMissingReplies(loaded + [entry(6, "agent") { $0.final = true }], summary: summary))
}

@Test func aCompleteOrSummarylessThreadDoesNotReload() {
    let loaded = [entry(1, "user"), tool(2), entry(3, "agent") { $0.final = true }, entry(4, "agent") { $0.text = "partial" }]
    #expect(!ThreadFeed.isMissingReplies(loaded, summary: ThreadSummary(count: 2, lastAt: 0, authors: [])))
    #expect(!ThreadFeed.isMissingReplies([], summary: nil))
    // The backfill turns an older reply final: the summary count rises, so the loaded thread reloads.
    #expect(ThreadFeed.isMissingReplies(loaded, summary: ThreadSummary(count: 3, lastAt: 0, authors: [])))
}

@Test func theChipCarriesWhoStartedAMirroredOneShot() {
    var s = summary(origin: "terminal")
    s.originLabel = "scheduled: nightly"
    let chip = ThreadFeed.chip(face: CrewFace(portrait: nil), tint: .neutral, count: 1, unread: 1, lastAt: 0, place: nil, yourTurn: false, summary: s)
    #expect(chip.origin == .terminal && chip.originLabel == "scheduled: nightly")
    let plain = ThreadFeed.chip(face: CrewFace(portrait: nil), tint: .neutral, count: 1, unread: 0, lastAt: 0, place: nil, yourTurn: false, summary: summary(origin: "terminal"))
    #expect(plain.originLabel == nil)
}

// MARK: rooms (agent conversations)

private func roomEntry(_ id: String, kind: String = "agent", ago: Int64 = 0, _ configure: (inout EntryData) -> Void = { _ in }) -> Entry {
    var data = EntryData()
    configure(&data)
    return Entry(id: id, seq: Int64(id.utf8.reduce(0) { $0 &* 31 &+ Int($1) } & 0xffff), botId: "room", threadId: nil, rev: 1, kind: kind,
                 turn: 1, data: data, createdAt: 1_000_000 - ago, updatedAt: 0)
}

private func run(_ why: String, left: Int = 4, opening: String = "o1") throws -> RoomConversation {
    let json = #"{"conversation":"cv1","chat":"room","budget":10,"left":\#(left),"state":"ended","endedWhy":"\#(why)","opening":"\#(opening)"}"#
    return try JSONDecoder().decode(RoomConversation.self, from: Data(json.utf8))
}

@Test func theEndLineGoesAfterTheLastReplyThatWasNotLate() throws {
    let entries = [
        roomEntry("o1", kind: "user", ago: 40) { $0.text = "Plan it" },
        roomEntry("a1", ago: 30) { $0.text = "one"; $0.final = true; $0.conversation = "cv1" },
        roomEntry("a2", ago: 20) { $0.text = "two"; $0.final = true; $0.conversation = "cv1" },
        roomEntry("a3", ago: 10) { $0.text = "late"; $0.final = true; $0.conversation = "cv1"; $0.late = true; $0.lateWhy = "stopped" },
    ]
    let items = ChatItem.build(entries)
    let placed = ThreadFeed.endLines(items, conversations: [try run("stopped", left: 6)])
    #expect(placed.keys.sorted() == ["a2"])
}

@Test func aRunNobodyAnsweredEndsUnderTheMessageThatOpenedIt() throws {
    let items = ChatItem.build([roomEntry("o1", kind: "user") { $0.text = "Plan it" }])
    #expect(ThreadFeed.endLines(items, conversations: [try run("stopped")]).keys.sorted() == ["o1"])
}

@Test func aRunWhoseMessagesArentLoadedOrNeedsNoLineGetsNone() throws {
    let items = ChatItem.build([roomEntry("a1") { $0.text = "x"; $0.final = true }])
    #expect(ThreadFeed.endLines(items, conversations: [try run("spent", opening: "gone")]).isEmpty)
    #expect(ThreadFeed.endLines(items, conversations: [try run("idle", opening: "a1")]).isEmpty)
}

@Test func aBotsPostIsItsOwnAuthorNotTheOwners() {
    let me = roomEntry("m1", kind: "user") { $0.text = "hi" }
    let grok = roomEntry("g1", kind: "user") { $0.text = "hello"; $0.sender = EntrySender(botId: "bg1", name: "Grok", provider: "grok") }
    let ada = roomEntry("a1") { $0.text = "yo"; $0.final = true; $0.author = "ada" }
    #expect(ThreadFeed.authorKey(me) == "user" && ThreadFeed.authorKey(grok) == "bot:bg1" && ThreadFeed.authorKey(ada) == "agent:ada")
    let items = ChatItem.build([me, grok, ada])
    let starts = items.compactMap { item -> Bool? in if case let .entry(_, groupStart) = item.kind { groupStart } else { nil } }
    #expect(starts == [true, true, true])
    // Only the owner's own message is "a message of yours" to the scroll.
    #expect(items.filter { if case .entry = $0.kind { true } else { false } }.map(\.isUserMessage) == [true, false, false])
}
