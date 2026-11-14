import KithKit
import Foundation

extension BotStore {
    private struct Cache: Codable {
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

    func loadCache() {
        // A cache from another app build still shows at once; the stamp mismatch makes the next
        // hello fetch everything again underneath it.
        guard let data = try? Data(contentsOf: cacheURL),
              let cache = try? JSONDecoder().decode(Cache.self, from: data) else { return }
        cacheStamp = cache.stamp ?? ""
        hostId = cache.hostId
        rev = cache.rev
        bots = Dictionary(cache.bots.map { ($0.id, $0) }, uniquingKeysWith: { a, _ in a })
        myRooms = Dictionary((cache.rooms ?? []).map { ($0.id, $0) }, uniquingKeysWith: { a, _ in a })
        entries = Dictionary(grouping: cache.entries, by: \.botId)
    }

    func scheduleSave() {
        guard !retired else { return }
        saveTask?.cancel()
        saveTask = Task {
            try? await Task.sleep(for: .seconds(2))
            if !Task.isCancelled { saveCache() }
        }
    }

    public func saveCache() {
        guard !retired else { return }
        // Keep the newest 200 entries per bot; older ones page in from the host. Local queued and
        // failed messages stay visible so they can be cancelled or retried after a relaunch.
        let kept = entries.values.flatMap { list in
            list.filter { !$0.id.hasPrefix("local-") }.suffix(200)
                + list.filter { $0.id.hasPrefix("local-") && ["waiting", "delivering", "failed"].contains($0.data.status) }
        }
        let cache = Cache(stamp: "\(Self.appBuild)/\(hello?.version ?? "")\(Self.cacheSchema)", hostId: hostId, rev: rev, bots: Array(bots.values), rooms: Array(myRooms.values), entries: kept)
        if let data = try? JSONEncoder().encode(cache) {
            try? data.write(to: cacheURL, options: .atomic)
        }
    }
}
