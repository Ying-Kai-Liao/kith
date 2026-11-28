import Foundation

/// One agent's routines, as the You > Routines page lists them.
public struct RoutineGroup: Identifiable, Hashable, Sendable {
    /// The bot id the routines belong to; "other" for routines whose agent is gone or unknown.
    public var id: String
    public var title: String
    public var routines: [Routine]
    public var isOther: Bool { id == RoutineGroup.otherId }
    public static let otherId = "other"

    /// One section per agent, by name; routines of an agent no longer there go last under "Other".
    /// `names` maps a bot id to the agent's display name; groups (rooms) have none and fall under "Other".
    public static func group(_ routines: [Routine], names: [String: String]) -> [RoutineGroup] {
        var byBot: [String: [Routine]] = [:]
        var other: [Routine] = []
        for routine in routines {
            if names[routine.botId] != nil { byBot[routine.botId, default: []].append(routine) } else { other.append(routine) }
        }
        let byName: (Routine, Routine) -> Bool = { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
        var groups = byBot.map { RoutineGroup(id: $0.key, title: names[$0.key] ?? $0.key, routines: $0.value.sorted(by: byName)) }
            .sorted { $0.title.localizedCaseInsensitiveCompare($1.title) == .orderedAscending }
        if !other.isEmpty { groups.append(RoutineGroup(id: otherId, title: "Other", routines: other.sorted(by: byName))) }
        return groups
    }
}

/// What one person spent across the owner's agents this calendar month.
public struct PersonTotal: Identifiable, Hashable, Sendable {
    public struct Share: Hashable, Sendable {
        public var agent: String
        public var cost: Double
    }
    public var id: String
    public var name: String
    public var total: Double
    /// The agents they spent on, costliest first.
    public var agents: [Share]

    /// The Agents | People split's second side: every agent's `agentPeople` summed per person, costliest first.
    /// The owner's own key is "You". People with no spend are left out.
    public static func combine(_ perAgent: [AgentPeople]) -> [PersonTotal] {
        var spent: [String: [Share]] = [:]
        for entry in perAgent {
            for (person, cost) in entry.people where cost > 0 { spent[person, default: []].append(Share(agent: entry.agent, cost: cost)) }
        }
        return spent.map { person, rows in
            PersonTotal(id: person, name: person == "owner" ? "You" : person, total: rows.reduce(0) { $0 + $1.cost },
                        agents: rows.sorted { $0.cost > $1.cost })
        }
        .sorted { $0.total != $1.total ? $0.total > $1.total : $0.name < $1.name }
    }
}

/// Which side of Usage's cost part is shown.
public enum SpendSplit: String, CaseIterable, Hashable, Sendable {
    case agents, people
    public var label: String { self == .agents ? "Agents" : "People" }
}

/// The plan meters of Usage, one set per account. The host reads each Claude Code / Codex account's windows
/// (`accounts` answer, host fact 0.1: `usage` = {readAt, windows} or null); an account without them, or a host
/// older than 0.1, falls back to today's one set per provider (`hello.usage`).
public struct PlanMeters: Hashable, Sendable {
    public struct Account: Identifiable, Hashable, Sendable {
        public var id: String
        /// Shown over the windows: "Claude", or "Claude · school" for a named account.
        public var title: String
        public var provider: UsageProvider
        /// When the host read this account's windows; nil for a provider-wide fallback.
        public var readAt: Date?
    }
    public var accounts: [Account]

    public init(accounts: [Account]) { self.accounts = accounts }

    /// Today: one account per provider the host reads.
    public init(_ usage: Usage) { self.init(usage, accounts: nil) }

    /// `accounts` is nil when the host didn't answer (older host, not the owner): the provider sets stand alone.
    public init(_ usage: Usage, accounts harness: [HarnessAccount]?) {
        var out: [Account] = []
        var covered: Set<String> = []
        for account in harness ?? [] where !account.windows.isEmpty {
            covered.insert(account.harness)
            let name = Self.providerName(account.harness)
            let title = account.account.isEmpty ? name : "\(name) · \(account.account)"
            let windows = account.windows.map { w in
                UsageWindow(id: w.name, label: Self.windowLabel(w.name), percent: w.percent ?? 0,
                            resetsAt: w.resetsAt.map { Int64($0.timeIntervalSince1970 * 1000) }, resetsText: nil)
            }
            let readAt = account.usageReadAt
            out.append(Account(id: account.id, title: title,
                               provider: UsageProvider(id: account.harness, name: title, windows: windows, source: account.harness,
                                                       updatedAt: readAt.map { Int64($0.timeIntervalSince1970 * 1000) } ?? 0),
                               readAt: readAt))
        }
        // Providers with no account reading: today's windows.
        for p in usage.providers where !covered.contains(p.id) {
            out.append(Account(id: p.id, title: p.name, provider: p, readAt: nil))
        }
        accounts = out
    }

    /// "just now", "5m ago", "3h ago", "2d ago".
    public static func readLabel(_ date: Date, now: Date = .now) -> String {
        let s = max(0, now.timeIntervalSince(date))
        switch s {
        case ..<60: return "just now"
        case ..<3600: return "\(Int(s / 60))m ago"
        case ..<86_400: return "\(Int(s / 3600))h ago"
        default: return "\(Int(s / 86_400))d ago"
        }
    }

    static func providerName(_ harness: String) -> String {
        switch harness {
        case "claude": "Claude"
        case "codex": "Codex"
        default: harness.prefix(1).uppercased() + harness.dropFirst()
        }
    }

    /// The host names a window by id; the page shows what the plan calls it.
    static func windowLabel(_ name: String) -> String {
        switch name {
        case "five_hour": "5-hour"
        case "seven_day": "Weekly"
        default: name.replacingOccurrences(of: "_", with: " ").capitalized
        }
    }
}
