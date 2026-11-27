import Foundation
import Observation

/// The owner's Feed: posts newest first, the stories ring above it, and the actions on a card (like, hide, reply).
/// Posts arrive by `feed` pages and by `post` events; an event replaces a post by id when its rev is newer.
@MainActor
@Observable
public final class FeedStore {
    public enum Phase: Equatable, Sendable {
        case idle, loading, ready
        /// Could not reach the computer and nothing is loaded.
        case offline(String)
        /// The computer is from before the Feed: show a short "update your Mac" note.
        case needsUpdate
    }

    public private(set) var posts: [HostContract.Post] = []
    public private(set) var stories: [HostContract.Story] = []
    public private(set) var phase: Phase = .idle
    public private(set) var hasMore = false
    public private(set) var loadingMore = false
    /// Posts with a reply on its way, so a second tap or a second send does nothing.
    public private(set) var replying: Set<String> = []
    /// A short line for an action that failed (never for "not yet").
    public var notice: String?

    @ObservationIgnored private var next: Int64?
    @ObservationIgnored private var likesPending: Set<String> = []
    @ObservationIgnored private var storiesTask: Task<Void, Never>?
    @ObservationIgnored private let host: @MainActor () -> (any FeedHost)?
    @ObservationIgnored private let chats: @MainActor () -> [Bot]
    @ObservationIgnored private let storyDebounce: Duration
    @ObservationIgnored private let pageSize: Int64

    public init(host: @escaping @MainActor () -> (any FeedHost)?, chats: @escaping @MainActor () -> [Bot],
                storyDebounce: Duration = .seconds(2), pageSize: Int64 = 30) {
        self.host = host
        self.chats = chats
        self.storyDebounce = storyDebounce
        self.pageSize = pageSize
    }

    // MARK: loading

    /// The newest page, when the Feed appears and on pull to refresh. Stories load with it.
    public func load() async {
        guard let host = host() else {
            if posts.isEmpty { phase = .offline("Not connected to your computer.") }
            return
        }
        if posts.isEmpty { phase = .loading }
        do {
            let page = try await host.feed(before: nil, limit: pageSize)
            posts = Self.ordered(page.posts.filter { !$0.hidden })
            next = page.next
            hasMore = page.next != nil
            phase = .ready
        } catch {
            if isUnknownMethod(error) { phase = .needsUpdate } else if posts.isEmpty { phase = .offline(error.localizedDescription) }
            return
        }
        await loadStories()
    }

    /// The page after the last one, when the list scrolls near its end.
    public func loadMore() async {
        guard let before = next, !loadingMore, let host = host() else { return }
        loadingMore = true
        defer { loadingMore = false }
        guard let page = try? await host.feed(before: before, limit: pageSize) else { return }
        let known = Set(posts.map(\.id))
        posts = Self.ordered(posts + page.posts.filter { !known.contains($0.id) && !$0.hidden })
        next = page.next
        hasMore = page.next != nil
    }

    public func loadStories() async {
        guard let host = host() else { return }
        do {
            stories = try await host.stories()
        } catch {
            // An older host has none; a failed read keeps the last rings, which expire on their own.
            if isUnknownMethod(error) || isNotYet(error) { stories = [] }
        }
    }

    /// An agent said something in some chat: the rings may have changed. Refetched at most once per quiet 2 s.
    public func entryArrived() {
        guard phase == .ready else { return }
        storiesTask?.cancel()
        let wait = storyDebounce
        storiesTask = Task { [weak self] in
            try? await Task.sleep(for: wait)
            guard !Task.isCancelled else { return }
            await self?.loadStories()
        }
    }

    /// The rings still lit at `now`: a ring ends three hours after the agent's last word.
    public func liveStories(now: Date = .now) -> [HostContract.Story] {
        let ms = Int64(now.timeIntervalSince1970 * 1000)
        return stories.filter { $0.isLive(nowMs: ms) }
    }

    // MARK: events

    /// A `post` event: a hidden post leaves, a newer rev replaces by id, an older one is ignored. A post that
    /// belongs below the pages not loaded yet waits for its page.
    public func apply(_ post: HostContract.Post) {
        guard phase == .ready else { return }
        if post.hidden {
            posts.removeAll { $0.id == post.id }
            return
        }
        if let i = posts.firstIndex(where: { $0.id == post.id }) {
            guard post.rev > posts[i].rev else { return }
            posts[i] = post
        } else {
            if hasMore, let last = posts.last, post.updatedAt < last.updatedAt { return }
            posts.append(post)
        }
        posts = Self.ordered(posts)
    }

    private static func ordered(_ posts: [HostContract.Post]) -> [HostContract.Post] {
        posts.enumerated().sorted { a, b in
            a.element.updatedAt != b.element.updatedAt ? a.element.updatedAt > b.element.updatedAt : a.offset < b.offset
        }.map(\.element)
    }

    // MARK: actions

    /// Likes or takes back the like at once and asks the computer; a refusal puts it back. A host that answers
    /// "not yet" (likes come later than the Feed) puts it back without a word.
    public func toggleLike(_ id: String) async {
        guard let i = posts.firstIndex(where: { $0.id == id }), !likesPending.contains(id), let host = host() else { return }
        let was = posts[i].liked
        posts[i].liked = !was
        likesPending.insert(id)
        defer { likesPending.remove(id) }
        do {
            let answer = try await host.likePost(id: id, liked: !was)
            if let j = posts.firstIndex(where: { $0.id == id }), answer.rev >= posts[j].rev { posts[j] = answer }
        } catch {
            if let j = posts.firstIndex(where: { $0.id == id }) { posts[j].liked = was }
            if !isNotYet(error) { notice = error.localizedDescription }
        }
    }

    public func hide(_ id: String) async {
        guard let i = posts.firstIndex(where: { $0.id == id }), let host = host() else { return }
        let post = posts.remove(at: i)
        do {
            _ = try await host.hidePost(id: id, hidden: true)
        } catch {
            if !posts.contains(where: { $0.id == id }) { posts = Self.ordered(posts + [post]) }
            if !isNotYet(error) { notice = error.localizedDescription }
        }
    }

    // MARK: replies

    /// The chat is gone from the roster (a roster that hasn't loaded says nothing is gone).
    public func isChatRemoved(_ post: HostContract.Post) -> Bool {
        let all = chats()
        return !all.isEmpty && !all.contains { $0.id == post.source.chat }
    }

    /// Who an @ chip can name in the post's chat: nobody in a 1:1, the members of a group.
    public func mentionable(in post: HostContract.Post) -> [Bot] {
        let all = chats()
        guard let chat = all.first(where: { $0.id == post.source.chat }), chat.isGroup else { return [] }
        return chat.members.compactMap { id in all.first { $0.id == id } }
    }

    public func chat(_ id: String) -> Bot? { chats().first { $0.id == id } }

    /// The agent's own chat bot: its face and name on a card or a ring.
    public func agentBot(_ agent: String) -> Bot? { chats().first { !$0.isGroup && $0.hqAgent == agent } }

    /// Sends into the post's chat and thread. False (the box keeps its text) when it did not go.
    public func reply(to post: HostContract.Post, text: String) async -> Bool {
        let body = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !body.isEmpty, !replying.contains(post.id), !isChatRemoved(post), let host = host() else { return false }
        replying.insert(post.id)
        defer { replying.remove(post.id) }
        do {
            try await host.sendText(botId: post.source.chat, threadId: post.source.thread, text: body, clientNonce: UUID().uuidString)
            return true
        } catch {
            notice = error.localizedDescription
            return false
        }
    }

    public func shot(for post: HostContract.Post, file: SharedFile) async -> Data? {
        guard let entry = post.source.entry, let host = host() else { return nil }
        return await host.shotBytes(entryId: entry, file: file)
    }
}

/// The host's answer from before a method has its body.
func isNotYet(_ error: Error) -> Bool {
    let text = error.localizedDescription.lowercased()
    return text.contains("not yet") || text.contains("unknown method")
}
