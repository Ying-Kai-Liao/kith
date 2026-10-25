import KithKit
import Observation

/// Settings › Keys: the owner's saved keys on one computer, grouped by service, with Remove access per agent. Only
/// names, agents and dates ever arrive here; there is no value to show.
@MainActor
@Observable
public final class KeysStore {
    public private(set) var keys: [KeyRecord] = []
    public private(set) var loaded = false
    public var error: String?
    /// "<profile>/<agent>" pairs whose removal is on its way.
    public private(set) var removing: Set<String> = []
    private let store: BotStore

    public init(store: BotStore) { self.store = store }

    public var groups: [(service: String, keys: [KeyRecord])] { KeyRecord.grouped(keys) }

    public func load() async {
        #if DEBUG
        if DemoScreens.active {
            if !loaded { keys = Self.demoKeys }
            loaded = true
            return
        }
        #endif
        do {
            keys = try await store.withLink { try await $0.listKeys() }
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
        loaded = true
    }

    public func remove(agent: String, from key: KeyRecord) async {
        let token = key.profile + "/" + agent
        guard !removing.contains(token) else { return }
        removing.insert(token)
        defer { removing.remove(token) }
        #if DEBUG
        if DemoScreens.active {
            try? await Task.sleep(for: .milliseconds(300))
            keys = keys.map { k in var k = k; if k.profile == key.profile { k.agents.removeAll { $0 == agent } }; return k }
            return
        }
        #endif
        do {
            try await store.withLink { try await $0.revokeKey(agent: agent, service: key.service, profile: key.profile) }
            await load()
        } catch {
            self.error = error.localizedDescription
        }
    }

    #if DEBUG
    /// Obvious fakes for the `keys` demo.
    static let demoKeys: [KeyRecord] = [
        KeyRecord(profile: "github/work", service: "github", account: "work", owner: "kai", vars: ["GITHUB_TOKEN"], agents: ["coder", "designer"],
                  lastUsed: "2026-10-25T08:12:00Z"),
        KeyRecord(profile: "github/personal", service: "github", account: "personal", owner: "kai", vars: ["GITHUB_TOKEN"], agents: ["personal"],
                  lastUsed: nil),
        KeyRecord(profile: "notion/team", service: "notion", account: "team", owner: "kai", vars: ["NOTION_TOKEN"], agents: ["designer"],
                  lastUsed: "2026-10-23T17:40:00Z"),
    ]
    #endif
}
