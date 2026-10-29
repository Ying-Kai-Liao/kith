import Foundation
import Testing
@testable import KithKit
@testable import KithUI

// The chat is flat (docs/design/DESIGN.md "Room chat"): every task's and thread's replies, and the tool calls, sit in
// the main chat in time order. The host keeps its task and thread model.

private func flat(_ id: String, _ kind: String, at: Int64, thread: String? = nil, text: String? = nil, final: Bool? = nil,
                  task: Bool? = nil) -> Entry {
    var data = EntryData(text: text)
    data.final = final
    data.task = task
    if kind == "tool" { data.toolKind = "execute"; data.title = "swift test" }
    return Entry(id: id, seq: at, botId: "b", threadId: thread, rev: at, kind: kind, turn: 1, data: data, createdAt: at * 1000, updatedAt: 0)
}

private func rowIds(_ items: [ChatItem]) -> [String] {
    items.compactMap {
        switch $0.kind {
        case .entry, .tool, .earlier: $0.id
        default: nil
        }
    }
}

@Test func theFlatChatInterleavesEveryThreadsRepliesAndToolsByTime() {
    let entries = [
        flat("t1", "user", at: 10, text: "Fix it", task: true),
        flat("c1", "agent", at: 25, text: "Side note", final: true),
        flat("r1", "tool", at: 12, thread: "t1"),
        flat("r2", "agent", at: 20, thread: "t1", text: "Fixed.", final: true),
        flat("t2", "user", at: 30, text: "Thanks", task: true),
        flat("r3", "agent", at: 40, thread: "t2", text: "Welcome", final: true),
        flat("th", "thought", at: 11, thread: "t1", text: "hm"),
    ]
    let items = ChatItem.build(entries, groupingExchanges: true)
    // Thinking stays in the trace; the task's roots are plain messages now.
    #expect(rowIds(items) == ["t1", "r1", "r2", "c1", "t2", "r3"])
    // Agent replies in a row share an author: only the first is a group start.
    let starts = items.compactMap { item -> Bool? in
        if case let .entry(e, start) = item.kind, e.id == "r2" || e.id == "c1" { start } else { nil }
    }
    #expect(starts == [true, false])
}

@Test func theFlatChatFoldsALongToolRunAndShowsEachThingOnce() {
    let tools = (1...7).map { flat("x\($0)", "tool", at: 100 + Int64($0), thread: "t1") }
    let reply = flat("done", "agent", at: 200, thread: "t1", text: "Done", final: true)
    // The reply arrives in the chat's entries and again from the thread fetch.
    let items = ChatItem.build([flat("t1", "user", at: 99, text: "Go", task: true)] + tools + [reply, reply], groupingExchanges: true)
    #expect(rowIds(items) == ["t1", "earlier-x1", "x4", "x5", "x6", "x7", "done"])
    guard case let .earlier(count) = items.first(where: { $0.id == "earlier-x1" })?.kind else { Issue.record("no fold"); return }
    #expect(count == 3)
}

@Test func aPendingCardInATasksThreadShowsInTheMainChat() {
    var card = flat("p1", "permission", at: 50, thread: "t1")
    card.data.status = "pending"
    let items = ChatItem.build([flat("t1", "user", at: 49, text: "Run it", task: true), card], groupingExchanges: true)
    #expect(rowIds(items) == ["t1", "p1"])
}

@Test func yourOwnTaskMessageKeepsItsRowIdInTheFlatChat() {
    var mine = flat("srv", "user", at: 5, text: "Hi", task: true)
    mine.data.clientNonce = "n1"
    #expect(ChatItem.build([mine], groupingExchanges: true).contains { $0.id == "user-n1" })
}

@Test func replyQuotesTheFirstLineAsABlockquote() {
    let message = flat("m", "agent", at: 1, text: "\n  First line  \nsecond line", final: true)
    #expect(ReplyQuote.line(of: message) == "First line")
    #expect(ReplyQuote.compose(quote: "First line", text: "Yes please") == "> First line\n\nYes please")
    #expect(ReplyQuote.compose(quote: nil, text: "Yes please") == "Yes please")
    #expect(ReplyQuote.line(of: flat("f", "agent", at: 1, final: true)) == nil)
    let long = flat("l", "agent", at: 1, text: String(repeating: "a", count: 300), final: true)
    #expect(ReplyQuote.line(of: long)?.count == ReplyQuote.maxLength + 1)
}
