import Foundation
import Observation
import Testing
@testable import KithKit
@testable import KithUI

/// One chat's live event redraws only the views reading that chat: the open chat reads its own entries, a roster row
/// reads its own bot's. Before the per-chat holders, `entries` was one dictionary and every reader fired for every event.

@MainActor private final class Fires {
    var count = 0
    func watch(_ read: @escaping @MainActor () -> Void) {
        withObservationTracking(read) { [self] in
            Task { @MainActor in self.count += 1; self.watch(read) }
        }
    }
}

private func entry(_ id: String, bot: String, seq: Int64) -> Entry {
    var data = EntryData(text: id)
    data.final = true
    return Entry(id: id, seq: seq, botId: bot, rev: seq, kind: "agent", turn: 0, data: data, createdAt: 1, updatedAt: 1)
}

@MainActor @Test func aLiveEventRedrawsOnlyItsOwnChat() async throws {
    let (storage, suite) = context()
    defer { storage.erase(); UserDefaults(suiteName: suite)?.removePersistentDomain(forName: suite) }
    let fake = FakeRemote(.ready(.relay))
    let store = BotStore(computer: randomComputer("Mac"), storage: storage) { fake }
    defer { store.retire() }
    store.setActive(true)
    #expect(await until { await fake.subscribed })
    await fake.emit(#"{"type":"hello","hostId":"h1","rev":2}"#)
    await fake.emit(#"{"type":"bot","bot":{"id":"a","name":"A","rev":1,"lastAt":1}}"#)
    await fake.emit(#"{"type":"bot","bot":{"id":"b","name":"B","rev":2,"lastAt":1}}"#)
    #expect(await until { store.bots.count == 2 })

    let openChat = Fires(), rowB = Fires(), everything = Fires()
    openChat.watch { _ = store.entries["a"] }
    rowB.watch { _ = store.entries["b"] }
    everything.watch { _ = store.entries.all }   // what every reader did while `entries` was one dictionary

    store.upsert(entry("b1", bot: "b", seq: 3))
    try await Task.sleep(for: .milliseconds(50))
    #expect(openChat.count == 0, "chat A's view stays put for chat B's event")
    #expect(rowB.count == 1)
    #expect(everything.count == 1)

    store.upsert(entry("a1", bot: "a", seq: 4))
    try await Task.sleep(for: .milliseconds(50))
    #expect(openChat.count == 1)
    #expect(rowB.count == 1, "chat B's row stays put for chat A's event")

    // Dropping a chat's entries tells its readers only.
    store.entries["a"] = nil
    try await Task.sleep(for: .milliseconds(50))
    #expect(openChat.count == 2 && rowB.count == 1)
    #expect(store.entries["a"] == nil && store.entries["b"]?.count == 1)
}

@MainActor @Test func theHomeBoardIgnoresChatsItDoesNotShow() async throws {
    let (storage, suite) = context()
    defer { storage.erase(); UserDefaults(suiteName: suite)?.removePersistentDomain(forName: suite) }
    let fake = FakeRemote(.ready(.relay))
    let store = BotStore(computer: randomComputer("Mac"), storage: storage) { fake }
    defer { store.retire() }
    store.setActive(true)
    #expect(await until { await fake.subscribed })
    await fake.emit(#"{"type":"hello","hostId":"h1","rev":3}"#)
    await fake.emit(#"{"type":"bot","bot":{"id":"a","name":"A","rev":1,"lastAt":1}}"#)
    await fake.emit(#"{"type":"bot","bot":{"id":"h","name":"H","rev":2,"lastAt":1,"hidden":true}}"#)
    #expect(await until { store.bots.count == 2 })
    store.upsert(entry("a0", bot: "a", seq: 3))
    store.upsert(entry("h0", bot: "h", seq: 4))

    let board = Fires(), prompts = Fires()
    board.watch { _ = BoardComputer(store: store, guestPrompts: []).entries }
    prompts.watch { _ = store.guestPrompts(isOwner: true) }

    store.upsert(entry("h1", bot: "h", seq: 5))   // a chat the roster hides
    try await Task.sleep(for: .milliseconds(50))
    #expect(board.count == 0, "the Board stays put for a chat it does not show")
    #expect(prompts.count == 0, "guest cards stay put for a chat with a bot")

    store.upsert(entry("a1", bot: "a", seq: 6))   // a chat the Board shows
    try await Task.sleep(for: .milliseconds(50))
    #expect(board.count == 1)
    #expect(prompts.count == 0, "a known bot's event never reaches the guest-card reader")

    store.upsert(entry("g1", bot: "stranger", seq: 7))   // a chat with no bot: a guest's
    try await Task.sleep(for: .milliseconds(50))
    #expect(prompts.count == 1)
    #expect(board.count == 1)
}

@MainActor @Test func upsertingIntoAChatIsInPlaceAndStaysLinear() {
    let shelf = EntryShelf()
    shelf["a", default: []].reserveCapacity(5000)
    shelf["a", default: []].append(entry("e0", bot: "a", seq: 0))
    let base = shelf["a", default: []].withUnsafeBufferPointer { $0.baseAddress }
    let start = ContinuousClock.now
    for i in 1..<4000 { shelf["a", default: []].append(entry("e\(i)", bot: "a", seq: Int64(i))) }
    let took = ContinuousClock.now - start
    #expect(shelf["a", default: []].withUnsafeBufferPointer { $0.baseAddress } == base, "appends reuse the chat's own storage")
    #expect(shelf["a", default: []].count == 4000)
    #expect(took < .seconds(2), "4000 appends into one chat took \(took); a copy per event is quadratic")
}
