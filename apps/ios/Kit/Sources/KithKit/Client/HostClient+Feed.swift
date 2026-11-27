import Foundation

/// What the Feed screen asks of the computer (serve/host/src/api/feed.rs, owner only). A host from before the Feed
/// answers "unknown method" (`isUnknownMethod`); one before likes or the route answers "not yet".
public protocol FeedHost: Sendable {
    func feed(before: Int64?, limit: Int64?) async throws -> HostContract.Feed
    func likePost(id: String, liked: Bool) async throws -> HostContract.Post
    func hidePost(id: String, hidden: Bool) async throws -> HostContract.Post
    func stories() async throws -> [HostContract.Story]
    func feedRoute(text: String) async throws -> HostContract.FeedRoute
    /// The existing `send`, for a reply in a post's chat and thread and for the bottom box.
    func sendText(botId: String, threadId: String?, text: String, clientNonce: String) async throws
    /// A shot's bytes (the file meta a post carries), read through the post's source entry.
    func shotBytes(entryId: String, file: SharedFile) async -> Data?
}

public extension HostClient {
    func feed(before: Int64?, limit: Int64?) async throws -> HostContract.Feed {
        try await call(.feed, HostContract.FeedParams(before: before, limit: limit))
    }

    func likePost(id: String, liked: Bool) async throws -> HostContract.Post {
        let res: HostContract.PostAnswer = try await call(.likePost, HostContract.LikePostParams(id: id, liked: liked))
        return res.post
    }

    func hidePost(id: String, hidden: Bool) async throws -> HostContract.Post {
        let res: HostContract.PostAnswer = try await call(.hidePost, HostContract.HidePostParams(id: id, hidden: hidden))
        return res.post
    }

    func stories() async throws -> [HostContract.Story] {
        let res: HostContract.Stories = try await call(.stories, HostContract.StoriesParams())
        return res.stories
    }

    func feedRoute(text: String) async throws -> HostContract.FeedRoute {
        try await call(.feedRoute, HostContract.FeedRouteParams(text: text))
    }

    func sendText(botId: String, threadId: String?, text: String, clientNonce: String) async throws {
        _ = try await send(botId: botId, text: text, clientNonce: clientNonce, threadId: threadId)
    }

    func shotBytes(entryId: String, file: SharedFile) async -> Data? {
        guard let id = UUID(uuidString: file.id) else { return nil }
        let cache = URL.cachesDirectory.appending(path: "kith-shared").appending(path: id.uuidString)
        return try? await FileDownload().bytes(client: self, entryId: entryId, file: file, cache: cache)
    }
}

extension HostClient: FeedHost {}
