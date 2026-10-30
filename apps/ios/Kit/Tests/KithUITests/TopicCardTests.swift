import Foundation
import SwiftUI
import Testing
@testable import KithKit
@testable import KithUI

private func message(_ id: String, _ kind: String, at: Int64, text: String? = nil, author: String? = nil, turn: Int64 = 1,
                     _ configure: (inout EntryData) -> Void = { _ in }) -> Entry {
    var data = EntryData(text: text)
    if kind == "agent" { data.final = true }
    data.author = author
    if kind == "tool" { data.toolKind = "execute"; data.title = "swift test"; data.status = "completed" }
    configure(&data)
    return Entry(id: id, seq: at, botId: "b", rev: at, kind: kind, turn: turn, data: data, createdAt: at * 1000, updatedAt: 0)
}

private func chat() -> [Entry] {
    [
        message("before", "user", at: 10, text: "Hello"),
        message("root", "user", at: 20, text: "What margin do we ship?") { $0.topic = EntryTopic(id: "t", title: "Margin target", untilAt: 40_000) },
        message("tool", "tool", at: 25),
        message("a1", "agent", at: 30, text: "Safe enough at 0.85.", author: "designer"),
        message("after", "user", at: 50, text: "Thanks"),
    ]
}

@Test func aTopicIsOneCardWhereItStartsAndItsEntriesLeaveTheChat() {
    let items = ChatItem.build(chat(), groupingExchanges: true)
    let kinds = items.compactMap { item -> String? in
        switch item.kind {
        case .entry(let e, _): e.id
        case .topic(let g): "topic:" + g.members.map(\.id).joined(separator: ",")
        default: nil
        }
    }
    #expect(kinds == ["before", "topic:root,tool,a1", "after"])
}

@Test func unfoldedOrOldHostsChatsStayFlat() {
    var entries = chat()
    entries[1].data.topic = nil
    let items = ChatItem.build(entries, groupingExchanges: true)
    #expect(!items.contains { if case .topic = $0.kind { true } else { false } })
}

@Test func anExpandedTopicBuildsItsRowsWithoutFoldingAndKeepsTheStepsRow() throws {
    let group = try #require(Topics.groups(in: chat()).first)
    let items = ChatItem.build(group.members, groupingExchanges: true, foldingTopics: false)
    #expect(items.contains { if case .steps = $0.kind { true } else { false } })
    #expect(!items.contains { if case .topic = $0.kind { true } else { false } })
}

@Test func searchLandingInsideATopicFindsItsCard() throws {
    let items = ChatItem.build(chat(), groupingExchanges: true)
    #expect(items.first { $0.holds(entry: "a1") }?.id == "topic-root")
}

/// Writes the card in light and dark to $TOPIC_SHOTS (default /tmp/topic-shots), for the PR.
@MainActor @Test func theCardRendersInLightAndDark() throws {
    let suite = "TopicCardTests.\(UUID().uuidString)"
    let storage = SharedStore.Context(accountID: UUID().uuidString, suite: suite)
    defer { UserDefaults(suiteName: suite)?.removePersistentDomain(forName: suite) }
    let store = BotStore(computer: Computer(id: "t", name: "Mac", signKey: ""), storage: storage) { throw HostError.unreachable }
    defer { store.retire() }
    store.bots["designer"] = try JSONDecoder().decode(Bot.self, from: Data(#"{"id":"designer","name":"designer"}"#.utf8))
    store.bots["b"] = try JSONDecoder().decode(Bot.self, from: Data(#"{"id":"b","name":"builder"}"#.utf8))
    var waiting = chat()
    waiting.append(message("ask", "permission", at: 35) { $0.status = "pending"; $0.title = "swift test"; $0.toolKind = "execute"; $0.options = [] })
    let dir = ProcessInfo.processInfo.environment["TOPIC_SHOTS"] ?? "/tmp/topic-shots"
    try FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
    for scheme in [ColorScheme.light, .dark] {
        for (name, entries, open) in [("collapsed", chat(), false), ("needs-you", waiting, false), ("expanded", chat(), true)] {
            let group = try #require(Topics.groups(in: entries).first)
            let view = VStack(spacing: 0) {
                TopicCard(group: group, chat: store.bots["b"], expanded: open, toggle: {}, rename: {}, unfold: {})
                if open {
                    ForEach(ChatItem.build(group.members, groupingExchanges: true, foldingTopics: false)) { item in
                        if case .entry(let e, let start) = item.kind { ChatRow(entry: e, groupStart: start, chat: store.bots["b"], openTrace: {}) }
                    }
                }
            }
            .frame(width: 390).padding(16).background(Palette.background)
            .environment(store).environment(\.colorScheme, scheme)
            let renderer = ImageRenderer(content: view)
            renderer.scale = 2
            let data = try #require(renderer.uiImage?.pngData())
            try data.write(to: URL(fileURLWithPath: "\(dir)/topic-\(name)-\(scheme == .dark ? "dark" : "light").png"))
        }
    }
}
