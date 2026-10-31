import KithKit
import Foundation
import Observation

/// The agent sheet's Access tab: this agent's keys, who may use it and what they spent. Owner only; a section the
/// host refuses (403) or doesn't have (404) is left out, and when none is left the tab goes.
@MainActor @Observable
final class AccessStore {
    enum Section<T> {
        case loading
        case loaded(T)
        /// 403 or 404: not shown.
        case hidden
        case failed(String)
    }

    let agent: String
    private let client: () async throws -> HostClient
    private(set) var keys: Section<[KeyRecord]> = .loading
    private(set) var ledger: Section<[AccessGrant]> = .loading
    private(set) var allowed: Section<AllowedList> = .loading
    private(set) var people: AgentPeople?
    /// Remove calls on their way: "key/<profile>" or "grant/<key>"; their rows are disabled.
    private(set) var removing: Set<String> = []
    private(set) var removeError: String?

    init(agent: String, client: @escaping () async throws -> HostClient) {
        self.agent = agent
        self.client = client
    }

    /// False once the host has refused everything: a member, or an older host.
    var available: Bool {
        if case .hidden = keys, case .hidden = ledger { return false }
        return true
    }

    var held: [KeyRecord] {
        if case .loaded(let all) = keys { return AccessTab.keys(all, for: agent) }
        return []
    }

    func load() async {
        #if DEBUG
        if DemoAccess.active(agent) {
            keys = .loaded(DemoAccess.keys); ledger = .loaded(DemoAccess.grants); people = DemoAccess.people
            allowed = .loaded(DemoAccess.allowed)
            return
        }
        #endif
        async let k: Void = loadKeys()
        async let l: Void = loadLedger()
        async let a: Void = loadAllowed()
        _ = await (k, l, a)
    }

    private func loadAllowed() async {
        do {
            allowed = .loaded(try await client().allowedRules(agent: agent))
        } catch where AccessTab.isUnavailable(error) {
            allowed = .hidden
        } catch {
            if case .loaded = allowed {} else { allowed = .failed(error.localizedDescription) }
        }
    }

    func remove(_ rule: AllowedRule) async {
        let token = "allowed/" + rule.id
        guard removing.insert(token).inserted else { return }
        removeError = nil
        defer { removing.remove(token) }
        do {
            try await client().removeAllowedRule(agent: agent, id: rule.id)
        } catch where AccessTab.isNotFound(error) {
            // A second tap, or already gone: the refreshed list is what is true now.
        } catch {
            removeError = error.localizedDescription
        }
        await loadAllowed()
    }

    private func loadKeys() async {
        do {
            keys = .loaded(try await client().listKeys())
        } catch where AccessTab.isUnavailable(error) {
            keys = .hidden
        } catch {
            if case .loaded = keys {} else { keys = .failed(error.localizedDescription) }
        }
    }

    private func loadLedger() async {
        do {
            let c = try await client()
            ledger = .loaded(try await c.accessLedger(agent: agent).grants)
            // Spend is a nicety: without it a person shows no budget line.
            people = try? await c.agentPeople(agent)
        } catch where AccessTab.isUnavailable(error) {
            ledger = .hidden
        } catch {
            if case .loaded = ledger {} else { ledger = .failed(error.localizedDescription) }
        }
    }

    func revoke(_ key: KeyRecord) async {
        let token = "key/" + key.profile
        guard removing.insert(token).inserted else { return }
        removeError = nil
        defer { removing.remove(token) }
        do {
            try await client().revokeKey(agent: agent, service: key.service, profile: key.profile)
            await loadKeys()
        } catch {
            removeError = error.localizedDescription
        }
    }

    func remove(_ grant: AccessGrant) async {
        let token = "grant/" + grant.key
        guard removing.insert(token).inserted else { return }
        removeError = nil
        defer { removing.remove(token) }
        do {
            try await client().removeGrant(grant)
            await loadLedger()
        } catch {
            removeError = error.localizedDescription
        }
    }
}

#if DEBUG
/// The `-kithDemoScreens agent-access` shot: a fixed keys list and ledger for the demo bot.
enum DemoAccess {
    static func active(_ agent: String) -> Bool { DemoScreens.active && agent == "coder" }
    static let keys = [
        KeyRecord(profile: "github/work", service: "github", account: "work", vars: ["GITHUB_TOKEN"], agents: ["coder"],
                  lastUsed: ISO8601DateFormatter().string(from: .now.addingTimeInterval(-3 * 3600))),
        KeyRecord(profile: "notion/sam", service: "notion", account: "sam", vars: ["NOTION_TOKEN"], agents: ["coder", "designer"], lastUsed: nil),
    ]
    static let grants = [
        AccessGrant(key: "g1", kind: "read", agent: "coder", person: "sam", name: "Sam", target: "~/docs", since: 1_760_000_000_000),
        AccessGrant(key: "g2", kind: "domain", agent: "coder", person: "sam", name: "Sam", target: "pypi.org"),
        AccessGrant(key: "g3", kind: "write", agent: "coder", person: "jo", name: "Jo", target: "~/notes"),
    ]
    static let allowed = AllowedList(allowed: [
        AllowedRule(id: "a1b2c3d4", rule: "Bash(ls /tmp)", reason: "[Data Exfiltration]",
                    added: ISO8601DateFormatter().string(from: .now.addingTimeInterval(-2 * 3600))),
    ])
    static let people = try? JSONDecoder().decode(AgentPeople.self, from: Data(
        #"{"agent":"coder","month":"2026-10","people":{"sam":4.2,"jo":0.8},"budgets":{"sam":20,"jo":null}}"#.utf8))
}
#endif
