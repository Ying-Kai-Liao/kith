import Foundation
import Testing
@testable import KithKit
@testable import KithUI

// The cached mirror is read off the main actor, so things can reach the store before it is in: a share sent at a cold
// launch, the queued-message check, a host that has already turned an invited person away.

private final class Tokens: InviteTokenStore, @unchecked Sendable {
    var invites: [ComputerID: StoredInvite] = [:]
    func stored(for computer: ComputerID) -> StoredInvite? { invites[computer] }
    func keep(_ invite: StoredInvite, for computer: ComputerID) throws { invites[computer] = invite }
    func forget(_ computer: ComputerID) { invites[computer] = nil }
}

private func entryJSON(_ id: String, seq: Int, status: String? = nil, nonce: String? = nil, kind: String = "agent") -> String {
    let extra = (status.map { #","status":"\#($0)""# } ?? "") + (nonce.map { #","clientNonce":"\#($0)""# } ?? "")
    return #"{"id":"\#(id)","seq":\#(seq),"botId":"dex","rev":\#(seq),"kind":"\#(kind)","turn":1,"data":{"text":"\#(id)","final":true\#(extra)},"createdAt":\#(seq),"updatedAt":1}"#
}

/// Writes a cache file for `computer` and returns where it is.
@discardableResult
@MainActor private func writeCache(_ storage: SharedStore.Context, _ computer: Computer, entries: [String], bots: String = "") throws -> URL {
    let build = Bundle.main.infoDictionary?["CFBundleVersion"] as? String ?? "0"
    let url = URL.cachesDirectory.appending(path: "kith-mirror-\(Bundle.main.bundleIdentifier ?? "app")-\(storage.id)-\(computer.id).json")
    try Data(#"{"stamp":"\#(build)/0.1.0\#(BotStore.cacheSchema)","hostId":"h1","rev":77,"bots":[\#(bots)],"entries":[\#(entries.joined(separator: ","))]}"#.utf8).write(to: url)
    return url
}

@MainActor @Test func aSendBeforeTheCacheIsReadKeepsTheCachedHistory() async throws {
    let (storage, suite) = context()
    defer { storage.erase(); UserDefaults(suiteName: suite)?.removePersistentDomain(forName: suite) }
    let computer = randomComputer("Mac")
    let url = try writeCache(storage, computer, entries: (1...5).map { entryJSON("e\($0)", seq: $0) },
                             bots: #"{"id":"dex","name":"Dex","rev":3}"#)
    defer { try? FileManager.default.removeItem(at: url) }
    let store = BotStore(computer: computer, storage: storage) { FakeRemote(.ready(.direct)) }
    defer { store.retire() }
    // The file is not in yet.
    #expect(!store.cacheLoaded)
    store.send("sent at launch", to: "dex")
    store.bots["live"] = try JSONDecoder().decode(Bot.self, from: Data(#"{"id":"live","name":"Live","rev":9}"#.utf8))
    await store.cacheReady()
    #expect(store.allEntries("dex").map(\.id).prefix(5) == ["e1", "e2", "e3", "e4", "e5"])
    #expect(store.allEntries("dex").filter { $0.id.hasPrefix("local-") }.count == 1)
    // The cached bot comes in and the one that arrived meanwhile stays.
    #expect(store.bots["dex"] != nil && store.bots["live"] != nil)
    #expect(store.rev == 77)
    // What is saved next is the whole cached chat (a "sending" message is not kept on disk), not a shortened one.
    store.saveCache()
    let saved = try JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: Any]
    #expect((saved?["entries"] as? [Any])?.count == 5)
}

@MainActor @Test func queuedMessagesInTheFileAreCheckedAfterItIsRead() async throws {
    let (storage, suite) = context()
    defer { storage.erase(); UserDefaults(suiteName: suite)?.removePersistentDomain(forName: suite) }
    let computer = randomComputer("Mac")
    let url = try writeCache(storage, computer, entries: [entryJSON("local-n1", seq: 1, status: "waiting", nonce: "n1", kind: "user")])
    defer { try? FileManager.default.removeItem(at: url) }
    let fake = FakeRemote(.ready(.relay))
    let store = BotStore(computer: computer, storage: storage) { fake }
    defer { store.retire() }
    // Called as the link comes up, before the file is necessarily in: the relay holds nothing, so the message failed.
    await store.reconcileQueued(fake)
    #expect(store.allEntries("dex").first { $0.id == "local-n1" }?.data.status == "failed")
}

@MainActor @Test func aHostThatTurnedAnInviteeAwayBeforeTheFileWasReadStillWipesIt() async throws {
    let (storage, suite) = context()
    defer { storage.erase(); UserDefaults(suiteName: suite)?.removePersistentDomain(forName: suite) }
    let computer = randomComputer("Mac")
    let url = try writeCache(storage, computer, entries: (1...3).map { entryJSON("e\($0)", seq: $0) },
                             bots: #"{"id":"dex","name":"Dex","rev":3}"#)
    defer { try? FileManager.default.removeItem(at: url) }
    let tokens = Tokens()
    tokens.invites[computer.id] = StoredInvite(inviteId: "inv_1", token: String(repeating: "a", count: 43))
    let store = BotStore(computer: computer, storage: storage) { FakeRemote(.ready(.direct)) }
    store.inviteTokens = tokens
    defer { store.retire() }
    // The refusal comes while the mirror is still empty, so nothing is wiped yet.
    store.setConnection(.unauthorized("Access was removed."))
    await store.cacheReady()
    #expect(store.bots.isEmpty)
    #expect(store.allEntries("dex").isEmpty)
    #expect(store.rev == 0)
}
