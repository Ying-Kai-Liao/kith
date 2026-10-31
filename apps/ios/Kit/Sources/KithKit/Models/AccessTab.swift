import Foundation

/// What the agent sheet's Access tab shows, from the answers of `listKeys`, `accessLedger` and `agentPeople`.
public enum AccessTab {
    /// The keys this agent holds, by service then account.
    public static func keys(_ keys: [KeyRecord], for agent: String) -> [KeyRecord] {
        keys.filter { $0.agents.contains(agent) }
            .sorted { ($0.service, $0.account) < ($1.service, $1.account) }
    }

    /// "github · work".
    public static func accountTitle(_ key: KeyRecord) -> String { "\(key.service) · \(key.account)" }

    /// "Keychain · last used 3 h ago", or "Keychain · not used yet".
    public static func keyDetail(_ key: KeyRecord, now: Date = .now, ago: (Date, Date) -> String) -> String {
        guard let date = key.lastUsedDate else { return "Keychain · not used yet" }
        return "Keychain · last used \(ago(date, now))"
    }

    public struct Person: Identifiable, Hashable, Sendable {
        /// The person's id (`*` is everyone).
        public var id: String
        public var name: String
        public var grants: [AccessGrant]
    }

    /// The ledger grouped by person, in order of first appearance (the host's order); a name from any of the person's rows.
    public static func people(_ grants: [AccessGrant]) -> [Person] {
        var order: [String] = []
        var byId: [String: Person] = [:]
        for grant in grants {
            let name = grant.person == "*" ? "Everyone" : (grant.name.isEmpty ? grant.person : grant.name)
            if byId[grant.person] == nil {
                order.append(grant.person)
                byId[grant.person] = Person(id: grant.person, name: name, grants: [])
            } else if byId[grant.person]?.name == grant.person, !grant.name.isEmpty {
                byId[grant.person]?.name = name
            }
            byId[grant.person]?.grants.append(grant)
        }
        return order.compactMap { byId[$0] }
    }

    /// The host's "Not found" for a rule that is already removed.
    public static func isNotFound(_ error: Error) -> Bool {
        guard case let HostError.http(_, message) = error else { return false }
        return message.localizedCaseInsensitiveContains("not found")
    }

    /// A 403 (not the owner) or 404 (an older host): the section is left out rather than shown as an error.
    public static func isUnavailable(_ error: Error) -> Bool {
        guard case let HostError.http(status, message) = error else { return false }
        return status == 403 || status == 404 || message.localizedCaseInsensitiveContains("unknown method")
    }
}
