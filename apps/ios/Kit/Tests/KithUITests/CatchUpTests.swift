import Foundation
import Observation
import Testing
@testable import KithKit
@testable import KithUI

/// What the events stream asks for and how often the chats redraw: a full catch-up is the expensive thing, so it must
/// happen only when the mirror really starts over, and land in a few redraws.

private func entryJSON(_ id: String, bot: String, seq: Int, rev: Int, text: String = "hi") -> String {
    #"{"type":"entry","entry":{"id":"\#(id)","seq":\#(seq),"botId":"\#(bot)","rev":\#(rev),"kind":"agent","turn":1,"data":{"text":"\#(text)","final":true},"createdAt":1,"updatedAt":1}}"#
}

/// Counts the changes a view reading `entries` would redraw for (SwiftUI coalesces the changes of one main-actor turn).
@MainActor private final class Redraws {
    var count = 0
    func watch(_ store: BotStore) {
        withObservationTracking { _ = store.entries.all } onChange: {
            Task { @MainActor in
                self.count += 1
                self.watch(store)
            }
        }
    }
}

@MainActor @Test func coldLaunchResumesFromTheCachedRev() async throws {
    let (storage, suite) = context()
    defer { storage.erase(); UserDefaults(suiteName: suite)?.removePersistentDomain(forName: suite) }
    let computer = randomComputer("Mac")
    let build = Bundle.main.infoDictionary?["CFBundleVersion"] as? String ?? "0"
    let url = URL.cachesDirectory.appending(path: "kith-mirror-\(Bundle.main.bundleIdentifier ?? "app")-\(storage.id)-\(computer.id).json")
    // Written by this app build against the same host version (FakeRemote's hello says 0.1.0).
    try Data(#"{"stamp":"\#(build)/0.1.0\#(BotStore.cacheSchema)","hostId":"h1","rev":77,"bots":[],"entries":[]}"#.utf8).write(to: url)
    let fake = FakeRemote(.ready(.relay))
    let store = BotStore(computer: computer, storage: storage) { fake }
    defer { store.retire() }
    store.setActive(true)
    #expect(await until { await fake.eventSince == [77] })
}

@MainActor @Test func reconnectsResumeFromTheRevReachedAndNeverStackStreams() async throws {
    let (storage, suite) = context()
    defer { storage.erase(); UserDefaults(suiteName: suite)?.removePersistentDomain(forName: suite) }
    let fake = FakeRemote(.ready(.relay))
    let store = BotStore(computer: randomComputer("Mac"), storage: storage) { fake }
    defer { store.retire() }
    store.setActive(true)
    #expect(await until { await fake.eventSince == [0] })
    await fake.emit(#"{"type":"hello","hostId":"h1","rev":5}"#)
    await fake.emit(#"{"type":"bot","bot":{"id":"b1","name":"Bot","rev":3,"lastAt":3}}"#)
    await fake.emit(entryJSON("e1", bot: "b1", seq: 1, rev: 5))
    #expect(await until { store.rev == 5 && store.caughtUp })

    // The relay drops and comes back.
    await fake.set(.connecting)
    await fake.set(.ready(.relay))
    #expect(await until { await fake.eventSince == [0, 5] })

    // The host's live queue overflowed.
    await fake.emit(#"{"type":"resync"}"#)
    #expect(await until { await fake.eventSince == [0, 5, 5] })
    #expect(await until { await fake.openEventStreams == 1 })

    // Control Center, the app switcher, a system prompt: the scene passes through `.inactive` and back.
    store.setActive(true)
    store.setActive(true)
    try await Task.sleep(for: .milliseconds(100))
    #expect(await fake.eventSince == [0, 5, 5])
    #expect(await fake.shutdownCount == 0)

    // Background and back: a new link, from the same rev.
    store.setActive(false)
    store.setActive(true)
    #expect(await until { await fake.eventSince == [0, 5, 5, 5] })
    #expect(await until { await fake.openEventStreams == 1 })
}

/// Kai's computer: 2,733 entries over 12 chats in the catch-up from rev 0. They redraw the chats a handful of times,
/// not once each, and the chat on screen gets a read receipt per batch, not per message.
@MainActor @Test func aFullCatchUpLandsInAFewRedraws() async throws {
    let (storage, suite) = context()
    defer { storage.erase(); UserDefaults(suiteName: suite)?.removePersistentDomain(forName: suite) }
    let fake = FakeRemote(.ready(.relay))
    let store = BotStore(computer: randomComputer("Mac"), storage: storage) { fake }
    defer { store.retire() }
    store.setActive(true)
    #expect(await until { await fake.subscribed })
    let chats = 12, perChat = 228
    let total = chats * perChat
    let text = String(repeating: "x", count: 1500)
    await fake.emit(#"{"type":"hello","hostId":"h1","rev":\#(total + chats)}"#)
    for c in 0..<chats { await fake.emit(#"{"type":"bot","bot":{"id":"b\#(c)","name":"Bot","rev":\#(c + 1),"lastAt":1}}"#) }
    #expect(await until { store.connection == .online })
    let token = UUID()
    store.setReading(token, botId: "b0", thread: nil, active: true)
    let readsBefore = await fake.calls.filter { $0 == "markRead" }.count
    let redraws = Redraws()
    redraws.watch(store)
    let start = ContinuousClock.now
    var rev = chats
    for i in 0..<perChat {
        for c in 0..<chats {
            rev += 1
            await fake.emit(entryJSON("e\(c)-\(i)", bot: "b\(c)", seq: rev, rev: rev, text: text))
        }
    }
    #expect(await until { store.allEntries("b11").count == perChat })
    let elapsed = ContinuousClock.now - start
    print("catch-up of \(total) entries: \(redraws.count) redraws, applied in \(elapsed)")
    #expect(store.caughtUp)
    #expect(redraws.count <= 40)
    try await Task.sleep(for: .milliseconds(200))
    let reads = await fake.calls.filter { $0 == "markRead" }.count - readsBefore
    // One per batch (and the one opening the chat sends), never one per message of the chat on screen.
    print("read receipts for the chat on screen: \(reads) for \(perChat) entries")
    #expect(reads <= 12)
}

private func row(_ id: String, bot: String = "b1", seq: Int64, thread: String? = nil) -> Entry {
    var data = EntryData(text: id)
    data.final = true
    return Entry(id: id, seq: seq, botId: bot, threadId: thread, rev: seq, kind: "agent", turn: 0, data: data, createdAt: 1, updatedAt: 1)
}

/// A catch-up brings an old task's root for its fresh replies; paging from that root would skip everything between.
@MainActor @Test func pagingStartsAtTheWindowNotAnOldTaskRoot() {
    let held = [row("root", seq: 10), row("a", seq: 200), row("b", seq: 210), row("reply", seq: 300, thread: "root")]
    #expect(BotStore.oldestWindowSeq(held) == 200)
    #expect(BotStore.oldestWindowSeq([row("root", seq: 10), row("reply", seq: 300, thread: "root")]) == 10)
    #expect(BotStore.oldestWindowSeq(held, upTo: 100) == 10)
}

/// 10,000 entries over 20 chats paged in: the cache, and so the next launch's decode, holds each chat's window only.
@MainActor @Test func theCacheKeepsEachChatsWindowHoweverMuchWasPagedIn() async throws {
    let (storage, suite) = context()
    defer { storage.erase(); UserDefaults(suiteName: suite)?.removePersistentDomain(forName: suite) }
    let computer = randomComputer("Mac")
    let store = BotStore(computer: computer, storage: storage) { FakeRemote(.ready(.direct)) }
    await store.cacheReady()
    var seq: Int64 = 0
    for c in 0..<20 {
        var list: [Entry] = []
        for i in 0..<500 {
            seq += 1
            // A task every 100 messages; a quarter of the chat is its replies.
            let root = "c\(c)-\(i / 100 * 100)"
            list.append(i % 4 == 3 ? row("c\(c)-\(i)", bot: "b\(c)", seq: seq, thread: root) : row("c\(c)-\(i)", bot: "b\(c)", seq: seq))
        }
        store.entries["b\(c)"] = list
    }
    store.saveCache()
    store.retire()
    let start = ContinuousClock.now
    let relaunched = BotStore(computer: computer, storage: storage) { FakeRemote(.ready(.direct)) }
    defer { relaunched.retire() }
    await relaunched.cacheReady()
    let held = relaunched.entries.values.map(\.count).reduce(0, +)
    print("cache after 10,000 entries over 20 chats: \(held) entries, loaded in \(ContinuousClock.now - start)")
    #expect(held <= 20 * (2 * BotStore.cachedPerChat + 5))
    for c in 0..<20 {
        let main = relaunched.chat("b\(c)")
        #expect(main.count >= BotStore.cachedPerChat && main.count <= BotStore.cachedPerChat + 5)
        #expect(main.last?.id == "c\(c)-498")
    }
}
