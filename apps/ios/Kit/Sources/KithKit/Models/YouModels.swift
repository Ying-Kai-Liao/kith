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

/// One account's plan meters (Claude Code and Codex windows). Today the host reports one set per provider
/// (`hello.usage`); the per-account meters of host fact 0.1 fill `accounts` from their own source, and the page
/// reads nothing else.
public struct PlanMeters: Hashable, Sendable {
    public struct Account: Identifiable, Hashable, Sendable {
        public var id: String
        /// Shown over the windows: the provider's name, or a named account once the host tells them apart.
        public var title: String
        public var provider: UsageProvider
    }
    public var accounts: [Account]

    public init(accounts: [Account]) { self.accounts = accounts }

    /// Today: one account per provider the host reads.
    public init(_ usage: Usage) {
        accounts = usage.providers.map { Account(id: $0.id, title: $0.name, provider: $0) }
    }
}
