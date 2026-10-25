import Foundation

/// Cost at API prices and monthly limits (serve/host/src/limits.rs). The computer's owner only; an older host answers
/// "unknown method" (`isUnknownMethod`), which hides the cost part of Usage.
public extension HostClient {
    /// The period's total, agents vs your own sessions, a row per agent.
    func usageReport(_ period: SpendPeriod) async throws -> UsageReport {
        try await call("usageReport", ["period": period.rawValue], timeout: 60)
    }

    /// One agent over `period` (its days, forecast and limit are always this month's).
    func agentUsage(_ agent: String, period: SpendPeriod = .month) async throws -> AgentUsage {
        try await call("agentUsage", ["agent": agent, "period": period.rawValue], timeout: 60)
    }

    /// Sets an agent's monthly limit (nil: no limit) and what happens at it; answers the agent's month.
    func setAgentLimit(_ agent: String, limit: Double?, at: AtLimit) async throws -> AgentUsage {
        struct Body: Encodable {
            var agent: String
            var limit: Double?
            var at: String
            func encode(to encoder: Encoder) throws {
                var c = encoder.container(keyedBy: CodingKeys.self)
                try c.encode(agent, forKey: .agent)
                // Always sent: null is "no limit", a missing key is "leave it".
                try c.encode(limit, forKey: .limit)
                try c.encode(at, forKey: .at)
            }
            enum CodingKeys: String, CodingKey { case agent, limit, at }
        }
        return try await call("setAgentLimit", Body(agent: agent, limit: limit, at: at.rawValue), timeout: 60)
    }

    /// What each person spent on the agent this month, and their budget on it.
    func agentPeople(_ agent: String) async throws -> AgentPeople {
        try await call("agentPeople", ["agent": agent], timeout: 60)
    }

    /// A person's budget on one agent, dollars a month; nil for none.
    func setPersonBudget(_ agent: String, person: String, budget: Double?) async throws -> AgentPeople {
        struct Body: Encodable {
            var agent: String
            var person: String
            var budget: Double?
            func encode(to encoder: Encoder) throws {
                var c = encoder.container(keyedBy: CodingKeys.self)
                try c.encode(agent, forKey: .agent)
                try c.encode(person, forKey: .person)
                try c.encode(budget, forKey: .budget)
            }
            enum CodingKeys: String, CodingKey { case agent, person, budget }
        }
        return try await call("setPersonBudget", Body(agent: agent, person: person, budget: budget), timeout: 60)
    }
}
