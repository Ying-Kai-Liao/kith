import KithKit
import Foundation

extension BotStore {
    private struct Cache: Codable, Sendable {
        /// App build + host version that wrote the cache; any change means refetch everything.
        var stamp: String?
        var hostId: String?
        var rev: Int64
        var bots: [Bot]
        /// An invited person's rooms with their stay: the first list after a launch closes what is gone and drops what a new stay replaced. Nil in an older cache.
        var rooms: [HostContract.MyRoom]?
        var entries: [Entry]
    }

    static let appBuild = Bundle.main.infoDictionary?["CFBundleVersion"] as? String ?? "0"
    /// Bumped when the cached shapes gain fields (v2: bot folder/project; v3: bot hqAgent; v4: entry answeredBy), so older caches refetch.
    /// The stamp uses appBuild, which may not change between dev builds.
    static let cacheSchema = "+v4"

    /// Per app, context and computer (`SharedStore.Context.erase()` removes a context's files).
    private var cacheURL: URL {
        URL.cachesDirectory.appending(path: "kith-mirror-\(Bundle.main.bundleIdentifier ?? "app")-\(storage.id)-\(computer.id).json")
    }

    /// What a launch reads from disk, ready to apply.
    private struct Loaded: Sendable {
        var cache: Cache
        var bots: [String: Bot]
        var rooms: [String: HostContract.MyRoom]
        var entries: [String: [Entry]]
    }

    /// Reads the mirror off the main actor: decoding a few MB of chats there held the first frames of a launch. The
    /// connection waits for it (`cacheReady`) before it asks for events, so it still resumes from the cached rev.
    func loadCache() {
        let url = cacheURL
        cacheLoad = Task { [weak self] in
            let loaded = await Task.detached(priority: .userInitiated) { Self.read(url) }.value
            guard let self else { return }
            if let loaded, !retired { apply(loaded) }
            finishLoad()
        }
    }

    /// Resolves once the cached mirror (if any) is in.
    func cacheReady() async { await cacheLoad?.value }

    private nonisolated static func read(_ url: URL) -> Loaded? {
        // A cache from another app build still shows at once; the stamp mismatch makes the next
        // hello fetch everything again underneath it.
        guard let data = try? Data(contentsOf: url), let cache = try? JSONDecoder().decode(Cache.self, from: data) else { return nil }
        return Loaded(
            cache: cache,
            bots: Dictionary(cache.bots.map { ($0.id, $0) }, uniquingKeysWith: { a, _ in a }),
            rooms: Dictionary((cache.rooms ?? []).map { ($0.id, $0) }, uniquingKeysWith: { a, _ in a }),
            entries: Dictionary(grouping: cache.entries, by: \.botId))
    }

    /// The file's mirror under what arrived while it was being read (a share sent at a cold launch, a chat started):
    /// memory is newer, so its entries go into the cached chat by the same rule an event does.
    private func apply(_ loaded: Loaded) {
        cacheStamp = loaded.cache.stamp ?? ""
        hostId = loaded.cache.hostId
        rev = max(rev, loaded.cache.rev)
        bots = loaded.bots.merging(bots) { _, live in live }
        myRooms = loaded.rooms.merging(myRooms) { _, live in live }
        var merged = loaded.entries
        for (chat, live) in entries.all {
            guard !live.isEmpty else { continue }
            var list = merged[chat] ?? []
            for e in live { Self.upsert(e, into: &list) }
            merged[chat] = list
        }
        entries.replaceAll(merged)
    }

    /// After the file is in: the host may already have turned this invited person away (the unauthorized answer came
    /// before there was a mirror to wipe), and what was just read must not outlive that.
    private func finishLoad() {
        cacheLoaded = true
        if case .unauthorized = connection, inviteTokens?.stored(for: computer.id) != nil {
            hello = nil
            resetMirror()
        }
        onRosterChanged?()
    }

    func scheduleSave() {
        guard !retired, cacheLoaded else { return }
        saveTask?.cancel()
        saveTask = Task {
            try? await Task.sleep(for: .seconds(2))
            guard !Task.isCancelled, !retired, let cache = snapshot() else { return }
            // Encoding a few MB of chats took the main thread long enough to stutter a scroll: it is written off it,
            // in order.
            let url = cacheURL
            Self.writer.async { Self.write(cache, to: url) }
        }
    }

    private static let writer = DispatchQueue(label: "com.yingkailiao.kith.mirror-cache", qos: .utility)

    /// Writes now, on this thread (leaving for the background, where the app may be suspended next).
    public func saveCache() {
        // Before the file is read the mirror is empty: writing it would replace the cache with nothing.
        guard !retired, cacheLoaded, let cache = snapshot() else { return }
        let url = cacheURL
        // After any write already queued, so it is not overwritten by an older one.
        Self.writer.sync { Self.write(cache, to: url) }
    }

    private nonisolated static func write(_ cache: Cache, to url: URL) {
        if let data = try? JSONEncoder().encode(cache) {
            try? data.write(to: url, options: .atomic)
        }
    }

    /// Entries per chat kept on disk: this many main-chat ones and this many thread replies, like the host's catch-up.
    static let cachedPerChat = 50

    /// A chat's newest main-chat entries and newest replies, with the roots of those replies, in the chat's order.
    static func cacheWindow(_ list: [Entry]) -> [Entry] {
        let main = list.filter { $0.threadId == nil }.suffix(cachedPerChat)
        let replies = list.filter { $0.threadId != nil }.suffix(cachedPerChat)
        let keep = Set(main.map(\.id)).union(replies.map(\.id)).union(replies.compactMap(\.threadId))
        return list.filter { keep.contains($0.id) }
    }

    private func snapshot() -> Cache? {
        guard !retired else { return nil }
        // Each chat's window (older ones page in from the host), so a launch reads the same few MB however much was
        // paged in. Local queued and failed messages stay visible so they can be cancelled or retried after a relaunch.
        let kept = entries.values.flatMap { list in
            Self.cacheWindow(list.filter { !$0.id.hasPrefix("local-") })
                + list.filter { $0.id.hasPrefix("local-") && ["waiting", "delivering", "failed"].contains($0.data.status) }
        }
        return Cache(stamp: "\(Self.appBuild)/\(hello?.version ?? "")\(Self.cacheSchema)", hostId: hostId, rev: rev, bots: Array(bots.values), rooms: Array(myRooms.values), entries: kept)
    }
}
