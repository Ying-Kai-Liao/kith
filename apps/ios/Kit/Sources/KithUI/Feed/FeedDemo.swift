#if DEBUG
import KithKit
import SwiftUI

/// A fake computer for previews and screenshots: invented posts, never anyone's real messages.
public final class FeedDemoHost: FeedHost, @unchecked Sendable {
    public var posts: [HostContract.Post]
    public var stories: [HostContract.Story]
    public init(posts: [HostContract.Post] = FeedDemo.posts(), stories: [HostContract.Story] = FeedDemo.stories()) {
        self.posts = posts
        self.stories = stories
    }

    public func feed(before: Int64?, limit: Int64?) async throws -> HostContract.Feed { .init(posts: posts) }
    public func likePost(id: String, liked: Bool) async throws -> HostContract.Post {
        throw NSError(domain: "demo", code: 1, userInfo: [NSLocalizedDescriptionKey: "not yet"])
    }
    public func hidePost(id: String, hidden: Bool) async throws -> HostContract.Post { posts[0] }
    public func stories() async throws -> [HostContract.Story] { stories }
    public func feedRoute(text: String) async throws -> HostContract.FeedRoute {
        throw NSError(domain: "demo", code: 2, userInfo: [NSLocalizedDescriptionKey: "not yet"])
    }
    public func sendText(botId: String, threadId: String?, text: String, clientNonce: String) async throws {}
    public func shotBytes(entryId: String, file: SharedFile) async -> Data? { nil }
}

public enum FeedDemo {
    static let now = Int64(Date().timeIntervalSince1970 * 1000)

    static func bot(_ id: String, _ name: String, color: String, group: [String] = []) -> Bot {
        var json: [String: Any] = ["id": id, "name": name, "kind": group.isEmpty ? "agent" : "group", "rev": 1, "status": "idle",
                                   "unread": 0, "members": group, "pinned": false, "lastAt": now, "avatarColor": color]
        if group.isEmpty { json["hqAgent"] = id }
        return try! JSONDecoder().decode(Bot.self, from: JSONSerialization.data(withJSONObject: json))
    }

    public static let bots = [bot("mira", "Mira", color: "kith:violet"), bot("otto", "Otto", color: "kith:teal"),
                              bot("juno", "Juno", color: "kith:sun"), bot("main", "Main", color: "kith:sky"),
                              bot("crew", "Studio", color: "", group: ["mira", "otto"])]

    static func post(_ id: String, _ agent: String, _ kind: String, _ title: String, _ line: String, minutes: Int64,
                     attachment: HostContract.JSONValue? = nil, items: [HostContract.JSONValue] = [], streak: Int64 = 0,
                     chat: String? = nil, liked: Bool = false) -> HostContract.Post {
        let at = now - minutes * 60_000
        return .init(id: id, rev: 1, agent: agent, kind: kind, title: title, line: line, attachment: attachment, items: items,
                     factAt: at, streak: streak, source: .init(chat: chat ?? agent, thread: nil, entry: "e-\(id)"), by: "agent",
                     createdAt: at, updatedAt: at, liked: liked, hidden: false)
    }

    public static func posts() -> [HostContract.Post] {
        func meta(_ name: String) -> HostContract.JSONValue {
            .object(["id": .string(UUID().uuidString), "name": .string(name), "size": .int(48_200), "sha256": .string(String(repeating: "a", count: 64))])
        }
        let page = meta("tide-chart.html"), list = meta("packing-list.pdf")
        return [
            post("p1", "otto", "merges", "Otto merged 4 today", "Docs and two small fixes went in.", minutes: 4,
                 items: ["#412 Fix the harbour map zoom", "#413 Trim the intro copy", "#414 Update the tide table", "#415 Rename the footer"].enumerated().map {
                     .object(["id": .string("studio#\(412 + $0.offset)"), "title": .string(String($0.element.dropFirst(5)))])
                 }),
            post("p2", "mira", "number", "Sign-ups this week", "Up on last week.", minutes: 25,
                 attachment: .object(["type": .string("number"), "value": .int(128), "unit": .string("people"), "delta": .int(14)]), liked: true),
            post("p3", "juno", "failing", "Nightly backup", "The disk was full again.", minutes: 70,
                 items: [.object(["id": .string("r3"), "at": .int(now - 70 * 60_000), "ok": .bool(false)]),
                         .object(["id": .string("r2"), "at": .int(now - 26 * 3_600_000), "ok": .bool(false)]),
                         .object(["id": .string("r1"), "at": .int(now - 50 * 3_600_000), "ok": .bool(false)])], streak: 3),
            post("p4", "mira", "page", "Tide chart for the week", "A page with the sailing windows.", minutes: 130,
                 attachment: .object(["type": .string("page"), "file": page, "shot": .null, "version": .string("v1")]), chat: "crew"),
            post("p5", "otto", "file", "Packing list", "One file, ready to print.", minutes: 300,
                 attachment: .object(["type": .string("file"), "file": list])),
            post("p6", "juno", "yourTurn", "Pick a lamp for the hallway", "Two options are waiting.", minutes: 600),
            post("p7", "main", "mystery", "Something new", "A kind this app does not know shows plainly.", minutes: 900,
                 attachment: .object(["type": .string("hologram")])),
        ]
    }

    public static func stories() -> [HostContract.Story] {
        [.init(chat: "mira", agent: "mira", lastAt: now - 600_000, lastEntryId: "e1", unread: 2),
         .init(chat: "otto", agent: "otto", lastAt: now - 3_600_000, lastEntryId: "e2", unread: 0),
         .init(chat: "juno", agent: "juno", lastAt: now - 9_000_000, lastEntryId: "e3", unread: 1)]
    }

    @MainActor
    public static func make(host: FeedDemoHost = FeedDemoHost()) -> (feed: FeedStore, composer: FeedComposer) {
        let feed = FeedStore(host: { host }, chats: { bots })
        let composer = FeedComposer(host: { host }, chats: { bots }, startChat: { id in bots.first { $0.id == id }! })
        return (feed, composer)
    }
}

private struct FeedPreviewHost: View {
    let parts = FeedDemo.make()
    var body: some View {
        NavigationStack { FeedView(feed: parts.feed, composer: parts.composer) { _, _ in } }
    }
}

#Preview("Feed, light") { FeedPreviewHost().preferredColorScheme(.light) }
#Preview("Feed, dark") { FeedPreviewHost().preferredColorScheme(.dark) }
#endif
