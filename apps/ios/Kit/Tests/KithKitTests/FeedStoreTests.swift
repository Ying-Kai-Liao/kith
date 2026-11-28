import Foundation
import Testing
@testable import KithKit

private final class FakeFeed: FeedHost, @unchecked Sendable {
    var pages: [[HostContract.Post]] = []
    var nextAfter: [Int64?] = []
    var calls: [Int64?] = []
    var storyCalls = 0
    var storiesAnswer: [HostContract.Story] = []
    var likeError: Error?
    var routeError: Error? = NSError(domain: "t", code: 1, userInfo: [NSLocalizedDescriptionKey: "not yet"])
    var routeAnswer = HostContract.FeedRoute(agent: "otto", why: "", others: [])
    var sent: [(bot: String, thread: String?, text: String)] = []
    var unknown = false

    func feed(before: Int64?, limit: Int64?) async throws -> HostContract.Feed {
        if unknown { throw NSError(domain: "t", code: 2, userInfo: [NSLocalizedDescriptionKey: "unknown method feed"]) }
        calls.append(before)
        let i = calls.count - 1
        return .init(posts: pages[i], next: nextAfter[i])
    }
    func likePost(id: String, liked: Bool) async throws -> HostContract.Post {
        if let likeError { throw likeError }
        var p = post(id, rev: 9, at: 100); p.liked = liked
        return p
    }
    func hidePost(id: String, hidden: Bool) async throws -> HostContract.Post { post(id, rev: 9, at: 100) }
    func stories() async throws -> [HostContract.Story] { storyCalls += 1; return storiesAnswer }
    func feedRoute(text: String) async throws -> HostContract.FeedRoute {
        if let routeError { throw routeError }
        return routeAnswer
    }
    func sendText(botId: String, threadId: String?, text: String, clientNonce: String) async throws { sent.append((botId, threadId, text)) }
    func shotBytes(entryId: String, file: SharedFile) async -> Data? { nil }
}

private func post(_ id: String, rev: Int64 = 1, at: Int64, hidden: Bool = false, chat: String = "mira") -> HostContract.Post {
    .init(id: id, rev: rev, agent: "mira", kind: "number", title: id, line: "", items: [], factAt: at, streak: 0,
          source: .init(chat: chat), by: "agent", createdAt: at, updatedAt: at, liked: false, hidden: hidden)
}

private func bot(_ id: String, group: [String] = []) -> Bot {
    var json: [String: Any] = ["id": id, "name": id.capitalized, "kind": group.isEmpty ? "agent" : "group", "rev": 1, "status": "idle",
                               "unread": 0, "members": group, "pinned": false, "lastAt": 0]
    if group.isEmpty { json["hqAgent"] = id }
    return try! JSONDecoder().decode(Bot.self, from: JSONSerialization.data(withJSONObject: json))
}

@MainActor
private func store(_ host: FakeFeed, chats: [Bot] = [], debounce: Duration = .milliseconds(20)) -> FeedStore {
    FeedStore(host: { host }, chats: { chats }, storyDebounce: debounce, pageSize: 2)
}

/// Polls until a debounced effect has happened; fixed sleeps flaked on the loaded CI simulator.
@MainActor private func settle(_ until: () -> Bool) async throws {
    for _ in 0..<500 where !until() { try await Task.sleep(for: .milliseconds(10)) }
}

@Suite @MainActor struct FeedStoreTests {
    @Test func loadsNewestFirstAndPages() async {
        let host = FakeFeed()
        host.pages = [[post("a", at: 30), post("b", at: 20)], [post("c", at: 10)]]
        host.nextAfter = [20, nil]
        let s = store(host)
        await s.load()
        #expect(s.posts.map(\.id) == ["a", "b"])
        #expect(s.hasMore)
        await s.loadMore()
        #expect(host.calls == [nil, 20])
        #expect(s.posts.map(\.id) == ["a", "b", "c"])
        #expect(!s.hasMore)
    }

    @Test func eventReplacesOnNewerRevOnly() async {
        let host = FakeFeed()
        host.pages = [[post("a", rev: 3, at: 30)]]; host.nextAfter = [nil]
        let s = store(host)
        await s.load()
        var older = post("a", rev: 2, at: 99); older.title = "old"
        s.apply(older)
        #expect(s.posts[0].title == "a")
        var newer = post("a", rev: 4, at: 40); newer.title = "new"
        s.apply(newer)
        #expect(s.posts[0].title == "new")
        s.apply(post("b", rev: 1, at: 50))
        #expect(s.posts.map(\.id) == ["b", "a"])
    }

    @Test func hiddenEventDropsThePost() async {
        let host = FakeFeed()
        host.pages = [[post("a", at: 30), post("b", at: 20)]]; host.nextAfter = [nil]
        let s = store(host)
        await s.load()
        s.apply(post("a", rev: 5, at: 30, hidden: true))
        #expect(s.posts.map(\.id) == ["b"])
    }

    @Test func eventBelowUnloadedPagesWaitsForItsPage() async {
        let host = FakeFeed()
        host.pages = [[post("a", at: 30), post("b", at: 20)]]; host.nextAfter = [20]
        let s = store(host)
        await s.load()
        s.apply(post("old", at: 5))
        #expect(s.posts.map(\.id) == ["a", "b"])
    }

    @Test func storyExpiresThreeHoursAfterLastWord() async {
        let now = Date(timeIntervalSince1970: 1_000_000)
        let ms = Int64(now.timeIntervalSince1970 * 1000)
        let host = FakeFeed()
        host.pages = [[]]; host.nextAfter = [nil]
        host.storiesAnswer = [.init(chat: "x", lastAt: ms - 3 * 3600_000 + 1000, lastEntryId: "e", unread: 0),
                              .init(chat: "y", lastAt: ms - 3 * 3600_000, lastEntryId: "e", unread: 0)]
        let s = store(host)
        await s.load()
        #expect(s.liveStories(now: now).map(\.chat) == ["x"])
        #expect(s.liveStories(now: now.addingTimeInterval(2)).isEmpty)
    }

    @Test func agentEntriesRefetchStoriesOnceAfterAQuietSpell() async throws {
        let host = FakeFeed()
        host.pages = [[]]; host.nextAfter = [nil]
        let s = store(host)
        await s.load()
        let base = host.storyCalls
        s.entryArrived(); s.entryArrived(); s.entryArrived()
        try await settle { host.storyCalls > base }
        // Once it has fired, give a second refetch time to show up before checking there is none.
        try await Task.sleep(for: .milliseconds(300))
        #expect(host.storyCalls == base + 1)
    }

    @Test func likeRevertsQuietlyWhenTheHostSaysNotYet() async {
        let host = FakeFeed()
        host.pages = [[post("a", at: 30)]]; host.nextAfter = [nil]
        host.likeError = NSError(domain: "t", code: 3, userInfo: [NSLocalizedDescriptionKey: "not yet"])
        let s = store(host)
        await s.load()
        await s.toggleLike("a")
        #expect(!s.posts[0].liked)
        #expect(s.notice == nil)
        host.likeError = NSError(domain: "t", code: 4, userInfo: [NSLocalizedDescriptionKey: "disk on fire"])
        await s.toggleLike("a")
        #expect(!s.posts[0].liked)
        #expect(s.notice == "disk on fire")
    }

    @Test func likeKeepsTheAnswer() async {
        let host = FakeFeed()
        host.pages = [[post("a", at: 30)]]; host.nextAfter = [nil]
        let s = store(host)
        await s.load()
        await s.toggleLike("a")
        #expect(s.posts[0].liked)
    }

    @Test func olderHostShowsTheUpdateNote() async {
        let host = FakeFeed()
        host.unknown = true
        let s = store(host)
        await s.load()
        #expect(s.phase == .needsUpdate)
    }

    @Test func replyGoesToTheSourceChatAndThread() async {
        let host = FakeFeed()
        var p = post("a", at: 30, chat: "crew"); p.source.thread = "t1"
        host.pages = [[p]]; host.nextAfter = [nil]
        let s = store(host, chats: [bot("mira"), bot("otto"), bot("crew", group: ["mira", "otto"])])
        await s.load()
        #expect(s.mentionable(in: p).map(\.id) == ["mira", "otto"])
        let ok = await s.reply(to: p, text: " @Otto hello ")
        #expect(ok)
        #expect(host.sent.first?.bot == "crew" && host.sent.first?.thread == "t1" && host.sent.first?.text == "@Otto hello")
    }

    @Test func removedChatHasNoReplyAndNoChips() async {
        let host = FakeFeed()
        let p = post("a", at: 30, chat: "gone")
        host.pages = [[p]]; host.nextAfter = [nil]
        let s = store(host, chats: [bot("mira")])
        await s.load()
        #expect(s.isChatRemoved(p))
        #expect(await s.reply(to: p, text: "hi") == false)
        #expect(host.sent.isEmpty)
        #expect(s.mentionable(in: post("b", at: 1, chat: "mira")).isEmpty)
    }
}

@Suite @MainActor struct FeedComposerTests {
    private func composer(_ host: FakeFeed, chats: [Bot], started: StartLog = StartLog()) -> FeedComposer {
        FeedComposer(host: { host }, chats: { chats }, startChat: { started.ids.append($0); return bot($0) },
                     home: { "main" }, debounce: .milliseconds(10))
    }
    final class StartLog { var ids: [String] = [] }

    @Test func untaggedSendWaitsForAPickForTheseWords() async throws {
        let host = FakeFeed()
        host.routeError = nil
        let c = composer(host, chats: [bot("otto"), bot("main")])
        c.edit("book a table")
        await c.send()
        #expect(host.sent.isEmpty)
        try await settle { c.target != nil }
        #expect(c.target == "otto")
        c.edit("book a table for two")
        #expect(c.target == nil)
        #expect(!c.canSend)
    }

    @Test func notYetFallsBackToTheHomeAgentChip() async throws {
        let host = FakeFeed()
        let c = composer(host, chats: [bot("main")])
        c.edit("hello")
        try await settle { c.pick != nil }
        #expect(c.pick?.agent == "main" && c.pick?.fallback == true)
        #expect(c.canSend)
    }

    @Test func tagSendsToExistingChatWithoutStartingOne() async {
        let host = FakeFeed()
        let log = StartLog()
        let c = composer(host, chats: [bot("otto")], started: log)
        c.edit("hi")
        c.choose("otto")
        await c.send()
        #expect(host.sent.first?.bot == "otto" && log.ids.isEmpty)
        #expect(c.text.isEmpty && c.tag == nil)
    }

    @Test func tagStartsAChatWhenThereIsNone() async {
        let host = FakeFeed()
        let log = StartLog()
        let c = composer(host, chats: [bot("otto")], started: log)
        c.edit("hi")
        c.choose("juno")
        await c.send()
        #expect(log.ids == ["juno"] && host.sent.first?.bot == "juno")
    }
}
