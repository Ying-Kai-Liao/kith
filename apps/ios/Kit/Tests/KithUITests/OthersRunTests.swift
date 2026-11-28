import Foundation
import Testing
@testable import KithKit
@testable import KithUI

private func entry(_ seq: Int64, _ kind: String, _ text: String = "", prompter: EntryPrompter? = nil,
                   status: String? = nil, style: String? = nil, unanswered: Bool? = nil) -> Entry {
    var data = EntryData(text: text, status: status)
    data.final = kind == "agent" ? true : nil
    data.prompter = prompter
    data.style = style
    data.unanswered = unanswered
    return Entry(id: "e\(seq)", seq: seq, botId: "helper", rev: seq, kind: kind, turn: 1, data: data,
                 createdAt: seq * 1000, updatedAt: seq * 1000)
}

private func items(_ entries: [Entry]) -> [ChatItem] {
    ChatItem.build(entries, groupingExchanges: true)
}

/// What the folded list is made of: `run(n)` for a folded run of n messages, else the entry id.
private func shape(_ items: [ChatItem]) -> [String] {
    items.compactMap { (item: ChatItem) -> String? in
        if case let .entry(e, _) = item.kind { return e.id }
        if case let .othersRun(group) = item.kind, case let .othersRun(_, count, _) = group.reason { return "run(\(count))" }
        return nil
    }
}

private let sam = EntryPrompter(kind: "person", name: "sam")
private let own = EntryPrompter(kind: "app")

@Test func oldEntriesWithoutPrompterNeverFold() {
    let list = items([entry(1, "user", "hi"), entry(2, "agent", "hello"), entry(3, "user", "more"), entry(4, "agent", "ok")])
    #expect(shape(OthersRun.fold(list, working: false)) == ["e1", "e2", "e3", "e4"])
}

@Test func ownPromptsAndTheirRepliesNeverFold() {
    let list = items([entry(1, "user", "a", prompter: own), entry(2, "agent", "b"),
                      entry(3, "user", "c", prompter: EntryPrompter(kind: "terminal")), entry(4, "agent", "d")])
    #expect(shape(OthersRun.fold(list, working: false)) == ["e1", "e2", "e3", "e4"])
}

@Test func aRunLastsUntilTheNextOwnPrompt() {
    let list = items([
        entry(1, "user", "a", prompter: own), entry(2, "agent", "b"),
        entry(3, "user", "from sam", prompter: sam), entry(4, "agent", "r1"),
        entry(5, "user", "from a manager", prompter: EntryPrompter(kind: "manager")), entry(6, "agent", "r2"),
        entry(7, "user", "mine again", prompter: own), entry(8, "agent", "r3"),
    ])
    #expect(shape(OthersRun.fold(list, working: false)) == ["e1", "e2", "run(4)", "e7", "e8"])
}

@Test func anUnknownPrompterEndsARunAndStaysVisible() {
    let list = items([entry(1, "user", "x", prompter: sam), entry(2, "agent", "y"),
                      entry(3, "user", "old"), entry(4, "agent", "z"),
                      entry(5, "user", "new kind", prompter: EntryPrompter(kind: "robot"))])
    #expect(shape(OthersRun.fold(list, working: false)) == ["run(2)", "e3", "e4", "e5"])
}

@Test func aRunOfOneMessageStillFolds() {
    let list = items([entry(1, "user", "x", prompter: EntryPrompter(kind: "schedule", name: "daily digest"))])
    let folded = OthersRun.fold(list, working: false)
    #expect(shape(folded) == ["run(1)"])
    guard case let .othersRun(group) = folded[0].kind, case let .othersRun(title, _, _) = group.reason else {
        Issue.record("no run")
        return
    }
    #expect(title == "Routine · daily digest")
    #expect(OthersRun.countText(1) == "1 message")
}

@Test func anAskInsideARunShowsUnderTheFoldLine() {
    var pending = entry(4, "permission", "run tests", status: "pending")
    pending.data.title = "cargo test"
    let list = items([entry(1, "user", "x", prompter: sam), entry(2, "agent", "r1"),
                      entry(3, "agent", "can you confirm?", unanswered: true), pending,
                      entry(5, "notice", "boom", style: "error"), entry(6, "agent", "r2")])
    #expect(shape(OthersRun.fold(list, working: false)) == ["run(3)", "e3", "e4", "e5"])
}

@Test func anAnsweredCardFoldsWithTheRun() {
    let list = items([entry(1, "user", "x", prompter: sam), entry(2, "permission", "ok", status: "answered"), entry(3, "agent", "r")])
    #expect(shape(OthersRun.fold(list, working: false)) == ["run(2)"])
}

@Test func theRunAtTheEndOfTheChatIsLiveWhileAgentWorksOrStreams() {
    let done = items([entry(1, "user", "x", prompter: EntryPrompter(kind: "agent", name: "coder")), entry(2, "agent", "r")])
    func live(_ list: [ChatItem], working: Bool) -> Bool {
        guard case let .othersRun(group) = OthersRun.fold(list, working: working)[0].kind,
              case let .othersRun(_, _, live) = group.reason else { return false }
        return live
    }
    #expect(!live(done, working: false))
    #expect(live(done, working: true))
    let streaming = items([entry(1, "user", "x", prompter: sam), entry(2, "agent", "wri", status: "streaming")])
    #expect(live(streaming, working: false))
}

@Test func newEntriesJoiningARunKeepItsRowAndUpdateTheCount() {
    let first = items([entry(1, "user", "x", prompter: sam), entry(2, "agent", "r")])
    let more = items([entry(1, "user", "x", prompter: sam), entry(2, "agent", "r"),
                      entry(3, "user", "y", prompter: sam), entry(4, "agent", "r2")])
    let a = OthersRun.fold(first, working: false)
    let b = OthersRun.fold(more, working: false)
    #expect(a[0].id == b[0].id)
    #expect(shape(a) == ["run(2)"])
    #expect(shape(b) == ["run(4)"])
}

@Test func aFoldedRunHoldsItsEntriesForJumpsAndSearch() {
    let folded = OthersRun.fold(items([entry(1, "user", "x", prompter: sam), entry(2, "agent", "r"),
                                       entry(3, "user", "mine", prompter: own)]), working: false)
    #expect(folded[0].holds(entry: "e2"))
    #expect(!folded[0].holds(entry: "e3"))
}

@Test func runTitles() {
    #expect(OthersRun.title([EntryPrompter(kind: "agent", name: "coder")], live: false) == "Worked with coder")
    #expect(OthersRun.title([EntryPrompter(kind: "manager")], live: false) == "Worked with a flow manager")
    #expect(OthersRun.title([sam], live: true) == "Working with sam")
    #expect(OthersRun.title([sam, EntryPrompter(kind: "agent", name: "writer")], live: false) == "Worked with sam and others")
    #expect(OthersRun.title([EntryPrompter(kind: "person")], live: false) == "Worked with someone")
}

@Test func prompterDecodesAndOldEntriesLackIt() throws {
    let with = try JSONDecoder().decode(EntryData.self, from: Data(#"{"text":"hi","prompter":{"kind":"schedule","name":"digest"}}"#.utf8))
    #expect(with.prompter == EntryPrompter(kind: "schedule", name: "digest"))
    let without = try JSONDecoder().decode(EntryData.self, from: Data(#"{"text":"hi"}"#.utf8))
    #expect(without.prompter == nil)
}
