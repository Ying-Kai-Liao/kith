import Foundation
import Testing
@testable import KithKit
@testable import KithUI

// Timings of the paths a scroll or a streamed token walks. The limits have headroom (a loaded simulator is several
// times slower than a Mac); they pin the order of magnitude. The printed TIMING lines are what the PR compares.

private func entry(_ i: Int, kind: String = "agent", text: String? = nil, thread: String? = nil, task: Bool? = nil) -> Entry {
    var data = EntryData(text: text ?? "Message \(i): some words about **things** and `code`, see [a link](https://example.com).")
    data.final = true
    data.task = task
    if kind == "tool" { data.toolKind = "execute"; data.title = "swift test"; data.status = "completed" }
    return Entry(id: "e\(i)", seq: Int64(i), botId: "b", threadId: thread, rev: Int64(i), kind: kind, turn: 1, data: data,
                 createdAt: Int64(i) * 60_000, updatedAt: 0)
}

/// A long chat as it really is: user and agent turns, runs of tool calls, a task with a thread now and then.
func longChat(_ n: Int) -> [Entry] {
    var out: [Entry] = []
    var i = 0
    while out.count < n {
        i += 1
        switch i % 10 {
        case 0: out.append(entry(i, kind: "user", task: true))
        case 1, 2, 3: out.append(entry(i, kind: "tool"))
        case 4: out.append(entry(i, kind: "tool", thread: "e\(i - 4)"))
        case 5: out.append(entry(i, kind: "user"))
        default: out.append(entry(i))
        }
    }
    return out
}

/// Best of `runs`, in milliseconds.
func time(_ label: String, runs: Int = 5, _ body: () -> Void) -> Double {
    var best = Double.infinity
    for _ in 0..<runs {
        let t = ContinuousClock().measure(body)
        best = min(best, Double(t.components.attoseconds) / 1e15 + Double(t.components.seconds) * 1e3)
    }
    let line = "TIMING \(label): \(String(format: "%.2f", best)) ms\n"
    print(line)
    // The test process's stdout does not reach xcodebuild's log; KITH_TIMING_LOG names a file to append to instead.
    if let path = ProcessInfo.processInfo.environment["KITH_TIMING_LOG"], let h = FileHandle(forWritingAtPath: path) {
        h.seekToEndOfFile(); h.write(Data(line.utf8)); try? h.close()
    }
    return best
}

@Test func buildingTheRowsOfALongChatIsFast() {
    let chat = longChat(5000)
    let ms = time("ChatItem.build 5000 entries") { _ = ChatItem.build(chat, groupingExchanges: true) }
    #expect(ms < 400)
}

// MARK: list and formatters

@Test func theCrewRowsOfAHundredFiftyChatsAreCheap() {
    let chats = (0..<150).map { _ in longChat(200) }
    let waiting = time("BoardModel.waitingCount x150 chats of 200") { for c in chats { _ = BoardModel.waitingCount(in: c) } }
    let now = Date()
    let days = time("RelativeTime.day x150") { for i in 0..<150 { _ = RelativeTime.day(now.addingTimeInterval(Double(-i) * 40_000), now: now) } }
    #expect(waiting < 400)
    #expect(days < 100)
}

@Test func chatSeparatorsAreCheap() {
    let now = Date()
    let ms = time("RelativeTime.separator x200") { for i in 0..<200 { _ = RelativeTime.separator(now.addingTimeInterval(Double(-i) * 4000)) } }
    #expect(ms < 100)
}

@MainActor @Test func aLaunchReadsTheCachedMirrorOffTheMainActor() async throws {
    let (storage, suite) = context()
    defer { storage.erase(); UserDefaults(suiteName: suite)?.removePersistentDomain(forName: suite) }
    let computer = randomComputer("Mac")
    let build = Bundle.main.infoDictionary?["CFBundleVersion"] as? String ?? "0"
    let url = URL.cachesDirectory.appending(path: "kith-mirror-\(Bundle.main.bundleIdentifier ?? "app")-\(storage.id)-\(computer.id).json")
    defer { try? FileManager.default.removeItem(at: url) }
    // The cache's window: 100 chats of ~100 entries, replies of a few hundred characters.
    let bots = (0..<100).map { #"{"id":"bot\#($0)","kind":"agent","name":"Bot \#($0)","status":"idle","lastAt":\#($0)}"# }.joined(separator: ",")
    let reply = String(repeating: "A reply with some words in it. ", count: 12)
    let entries = (0..<100).flatMap { b in
        (0..<100).map { #"{"id":"b\#(b)e\#($0)","seq":\#($0),"botId":"bot\#(b)","rev":\#($0),"kind":"agent","turn":1,"data":{"text":"\#(reply)","final":true},"createdAt":\#($0 * 1000),"updatedAt":1}"# }
    }.joined(separator: ",")
    try Data(#"{"stamp":"\#(build)/0.1.0\#(BotStore.cacheSchema)","hostId":"h1","rev":77,"bots":[\#(bots)],"entries":[\#(entries)]}"#.utf8).write(to: url)
    var store: BotStore?
    // What the main actor pays at launch: before, this decoded and grouped the whole file; now it starts a task.
    let ms = time("BotStore init with a 10k-entry cache (main actor)", runs: 1) {
        store = BotStore(computer: computer, storage: storage) { FakeRemote(.ready(.relay)) }
    }
    defer { store?.retire() }
    #expect(ms < 30)
    #expect(store?.cacheLoaded == false)
    await store?.cacheReady()
    #expect(store?.cacheLoaded == true)
    #expect(store?.bots.count == 100)
    #expect(store?.allEntries("bot7").count == 100)
    #expect(store?.rev == 77)
}

@Test func buildingTheRowsOfAShortChatIsFast() {
    let chat = longChat(300)
    let ms = time("ChatItem.build 300 entries") { _ = ChatItem.build(chat, groupingExchanges: true) }
    #expect(ms < 40)
}

/// Replies are markdown: headings, a table, a code block, long paragraphs, to ~20k characters.
func longMarkdown(_ chars: Int) -> String {
    var s = ""
    var i = 0
    while s.count < chars {
        i += 1
        s += "## Section \(i)\n\nA paragraph with **bold**, _italic_, `inline code` and a [link](https://example.com/\(i)). It goes on for a while so that it wraps over a few lines on a phone.\n\n"
        s += "- item one\n- item **two**\n  - nested `three`\n\n| a | b | c |\n|---|---|---|\n| 1 | **2** | 3 |\n| 4 | 5 | `6` |\n\n"
        s += "```swift\nlet x = \(i)\nprint(x)\n```\n\n> a quote\n\n"
    }
    return s
}

@Test func parsingALongEntryIsFast() {
    let source = longMarkdown(20_000)
    let blocks = MarkdownBlocks.parse(source)
    let parse = time("MarkdownBlocks.parse 20k chars") { _ = MarkdownBlocks.parse(source) }
    let inline = time("MarkdownText.inline over all paragraphs (\(blocks.count) blocks)") {
        for case let .paragraph(t) in blocks { _ = MarkdownText.inline(t) }
    }
    #expect(parse < 100)
    #expect(inline < 200)
}

/// A scroll back over rows already seen must not parse their markdown again.
@Test func theSameMarkdownIsParsedOnce() {
    // Unique text, so a parallel test's own markdown never lands in the same cache entries.
    let source = "unique \(UUID().uuidString)\n\n" + longMarkdown(20_000)
    var parses = 0
    let counted: (String) -> [MarkdownBlocks.Block] = { parses += 1; return MarkdownBlocks.parse($0) }
    let first = time("MarkdownCache.blocks first (parse)", runs: 1) { _ = MarkdownCache.blocks(source, parse: counted) }
    let again = time("MarkdownCache.blocks again (cached)") { _ = MarkdownCache.blocks(source, parse: counted) }
    #expect(parses == 1)
    #expect(again < first / 4 + 0.05)

    let paragraphs = MarkdownCache.blocks(source).compactMap { if case let .paragraph(t) = $0 { t } else { nil } }
    var inlines = 0
    let countedInline: (String) -> AttributedString = { inlines += 1; return AttributedString($0) }
    _ = time("MarkdownCache.inline all paragraphs (first pass parses, the rest hit)", runs: 5) {
        for t in paragraphs { _ = MarkdownCache.inline("u\(source.count)" + t, parse: countedInline) }
    }
    // Five passes over the paragraphs parsed each distinct one once.
    #expect(inlines == Set(paragraphs).count)
}
