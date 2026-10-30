import Foundation
import Testing
@testable import KithKit

private func entry(_ id: String, _ kind: String = "user", at: Int64, turn: Int64 = 1, thread: String? = nil,
                   _ configure: (inout EntryData) -> Void = { _ in }) -> Entry {
    var data = EntryData()
    configure(&data)
    return Entry(id: id, seq: at, botId: "b", threadId: thread, rev: 1, kind: kind, turn: turn, data: data, createdAt: at, updatedAt: at)
}

private func root(at: Int64 = 100, until: Int64 = 200, turns: [Int64] = []) -> Entry {
    entry("root", at: at) {
        $0.text = "Pick a margin\nsecond line"
        $0.topic = EntryTopic(id: "t", title: "Margin", untilId: "x", untilAt: until, turns: turns, createdAt: 300)
    }
}

private func folded(_ entries: [Entry]) -> [String] {
    Topics.groups(in: entries).first?.members.map(\.id) ?? []
}

@Test func entriesInRangeFoldAndOthersDoNot() {
    let all = [entry("before", at: 99), root(), entry("in", at: 150), entry("edge", at: 200), entry("after", at: 201)]
    #expect(folded(all) == ["root", "in", "edge"])
}

@Test func theSameMillisecondCountsAsInRange() {
    let all = [root(), entry("same", "agent", at: 100) { $0.final = true }]
    #expect(folded(all) == ["root", "same"])
}

@Test func aListedTurnFoldsItsTailAfterTheRoot() {
    let all = [root(turns: [7]), entry("tail", "agent", at: 500, turn: 7), entry("other", at: 500, turn: 8),
               entry("earlier", at: 50, turn: 7)]
    #expect(folded(all) == ["root", "tail"])
}

@Test func threadRepliesAndToolRowsFoldWithTheFlatView() {
    let all = [root(), entry("reply", at: 120, thread: "somewhere"), entry("tool", "tool", at: 130)]
    #expect(folded(all) == ["root", "reply", "tool"])
}

@Test func withoutALoadedRootNothingFolds() {
    #expect(Topics.groups(in: [entry("a", at: 150), entry("b", at: 160)]).isEmpty)
}

@Test func aRootInsideAnEarlierTopicIsNotATopic() {
    var inner = entry("inner", at: 150)
    inner.data.topic = EntryTopic(id: "t2", title: "Inner", untilAt: 400)
    let groups = Topics.groups(in: [root(), inner])
    #expect(groups.count == 1)
    #expect(groups[0].members.map(\.id) == ["root", "inner"])
}

@Test func theCardShowsFacesLatestLineCountAndWhatWaits() {
    let all = [
        root(),
        entry("a", "agent", at: 120) { $0.final = true; $0.author = "designer"; $0.text = "Safe enough at 0.85.\nmore" },
        entry("tool", "tool", at: 130),
        entry("p", "permission", at: 140) { $0.status = "pending" },
        entry("answered", "permission", at: 150) { $0.status = "answered" },
    ]
    let summary = Topics.summary(of: Topics.groups(in: all)[0])
    #expect(summary.authors == ["user", "agent:designer"])
    #expect(summary.latestLine == "Safe enough at 0.85.")
    #expect(summary.latest?.id == "a")
    #expect(summary.count == 2)
    #expect(summary.date == Date(milliseconds: 150))
    #expect(summary.needsYou.map(\.id) == ["p"])
}

@Test func theSuggestedTitleIsTheFirstLineTrimmedToEighty() {
    #expect(Topics.suggestedTitle(for: root()) == "Pick a margin")
    let long = entry("l", at: 1) { $0.text = String(repeating: "x", count: 120) }
    #expect(Topics.suggestedTitle(for: long).count == 80)
}

@Test func aTopicDecodesFromTheHostsEntry() throws {
    let json = #"{"id":"t","title":"Margin","untilId":"x","untilAt":200,"turns":[3],"createdAt":300}"#
    let topic = try JSONDecoder().decode(EntryTopic.self, from: Data(json.utf8))
    #expect(topic.turns == [3] && topic.untilAt == 200)
}
