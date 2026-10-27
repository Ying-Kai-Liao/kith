import Foundation

/// What things cost at API prices (never a bill) and the monthly limits on agents and people: the host's `usageReport`,
/// `agentUsage` and `agentPeople` (serve/host/src/limits.rs, from kernel/bin/kith-cost), and the at-limit cards it adds.
/// The words are decided here, the same as the desktop's `shared/usage-limits.ts`.

public enum SpendPeriod: String, Codable, Hashable, Sendable, CaseIterable {
    case today, week, month
}

/// What happens when an agent reaches its limit: a card (ask), a line (tell), or its runs wait (hold).
public enum AtLimit: String, Codable, Hashable, Sendable, CaseIterable, Identifiable {
    case ask, tell, hold
    public var id: String { rawValue }

    public var title: String {
        switch self {
        case .ask: "Ask me"
        case .tell: "Tell me and keep going"
        case .hold: "Hold its runs"
        }
    }

    public var detail: String {
        switch self {
        case .ask: "A card: allow this run, raise the limit, or stop"
        case .tell: "A line on Board; nothing waits"
        case .hold: "They wait until next month or until you raise it"
        }
    }

    /// The row's short value: "Ask me", "Tell me", "Hold its runs".
    public var short: String { self == .tell ? "Tell me" : title }

    /// Anything the host doesn't name is the default, ask.
    public init(loose: String?) { self = AtLimit(rawValue: loose ?? "") ?? .ask }
}

public struct UsageRow: Decodable, Hashable, Sendable, Identifiable {
    public var agent: String
    public var cost: Double
    public var runs: Int
    public var perRun: Double
    public var limit: Double?
    public var at: AtLimit
    /// This calendar month's spend, when it has a limit (what the bar measures).
    public var month: Double?
    public var id: String { agent }

    enum CodingKeys: String, CodingKey { case agent, cost, runs, perRun, limit, at, month }
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        agent = try c.decode(String.self, forKey: .agent)
        cost = try c.decodeIfPresent(Double.self, forKey: .cost) ?? 0
        runs = try c.decodeIfPresent(Int.self, forKey: .runs) ?? 0
        perRun = try c.decodeIfPresent(Double.self, forKey: .perRun) ?? 0
        limit = try c.decodeIfPresent(Double.self, forKey: .limit)
        at = AtLimit(loose: try c.decodeIfPresent(String.self, forKey: .at))
        month = try c.decodeIfPresent(Double.self, forKey: .month)
    }
}

/// Profile > Usage: the period's total at API prices, agents vs your own sessions, a row per agent.
public struct UsageReport: Decodable, Hashable, Sendable {
    public var period: SpendPeriod
    /// "Today", "Last 7 days", "October".
    public var label: String
    public var total: Double
    public var agents: Double
    public var own: Double
    public var rows: [UsageRow]
}

public struct SpendForecast: Decodable, Hashable, Sendable {
    public var perDay: Double
    public var since: String
    public var text: String
    /// The day each preset amount is reached at this pace (nil: not this month).
    public var reach: [String: String?]
}

public struct CostlyRun: Decodable, Hashable, Sendable, Identifiable {
    public var session: String
    public var cost: Double
    /// "2026-10-24 00:04:08".
    public var started: String
    public var via: String?
    public var turn: String?
    public var id: String { "\(session)/\(started)" }

    /// The run's first line, short.
    public var title: String {
        let line = (turn ?? "").split(separator: "\n").map { $0.trimmingCharacters(in: .whitespaces) }.first { !$0.isEmpty } ?? "A run"
        return line.count > 60 ? "\(line.prefix(59))…" : line
    }
}

public struct ModelSpend: Decodable, Hashable, Sendable {
    public var model: String
    public var effort: String?
    public var cost: Double
    public var runs: Int
}

/// One agent: its total, days, forecast, models and costliest runs.
public struct AgentUsage: Decodable, Hashable, Sendable {
    public struct Day: Decodable, Hashable, Sendable { public var day: String; public var cost: Double }
    public var agent: String
    public var period: SpendPeriod
    public var label: String
    public var cost: Double
    public var runs: Int
    public var perRun: Double
    /// This calendar month's spend and its name.
    public var month: Double
    public var monthLabel: String
    public var limit: Double?
    public var at: AtLimit
    public var days: [Day]
    public var models: [ModelSpend]
    public var costliest: [CostlyRun]
    public var forecast: SpendForecast?

    enum CodingKeys: String, CodingKey { case agent, period, label, cost, runs, perRun, month, monthLabel, limit, at, days, models, costliest, forecast }
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        agent = try c.decode(String.self, forKey: .agent)
        period = (try? c.decode(SpendPeriod.self, forKey: .period)) ?? .month
        label = try c.decodeIfPresent(String.self, forKey: .label) ?? ""
        cost = try c.decodeIfPresent(Double.self, forKey: .cost) ?? 0
        runs = try c.decodeIfPresent(Int.self, forKey: .runs) ?? 0
        perRun = try c.decodeIfPresent(Double.self, forKey: .perRun) ?? 0
        month = try c.decodeIfPresent(Double.self, forKey: .month) ?? 0
        monthLabel = try c.decodeIfPresent(String.self, forKey: .monthLabel) ?? ""
        limit = try c.decodeIfPresent(Double.self, forKey: .limit)
        at = AtLimit(loose: try c.decodeIfPresent(String.self, forKey: .at))
        days = try c.decodeIfPresent([Day].self, forKey: .days) ?? []
        models = try c.decodeIfPresent([ModelSpend].self, forKey: .models) ?? []
        costliest = try c.decodeIfPresent([CostlyRun].self, forKey: .costliest) ?? []
        forecast = try? c.decodeIfPresent(SpendForecast.self, forKey: .forecast)
    }
}

/// Access > People: what each person spent on the agent this month, and their budget on it.
public struct AgentPeople: Decodable, Hashable, Sendable {
    public var agent: String
    public var month: String
    /// What each person's runs cost this month; `owner` is the owner's own.
    public var people: [String: Double]
    /// Each person's budget on the agent, dollars a month; nil for none.
    public var budgets: [String: Double?]

    public func spend(of person: String) -> (spent: Double, budget: Double?) {
        (people[person] ?? 0, budgets[person] ?? nil)
    }

    /// "$20.10 of $20", or what they spent when there is no limit.
    public func line(for person: String) -> String {
        let s = spend(of: person)
        return s.budget.map { Spend.of(s.spent, $0, cents: true) } ?? "\(Spend.money(s.spent, cents: true)) this month"
    }
}

/// The owner's bots together, on every agent (`botsCap`): their monthly cap, nil for none, and what they spent.
public struct BotsCap: Codable, Hashable, Sendable {
    public var month: String
    public var cap: Double?
    public var spent: Double

    /// "$3.00 of $50", or what they spent when there is no cap.
    public var line: String {
        cap.map { Spend.of(spent, $0, cents: true) } ?? "\(Spend.money(spent, cents: true)) this month"
    }
}

/// The limit a card carries (serve/host/src/limits.rs agent_card / guest_card / bots_card).
public struct LimitCardData: Codable, Hashable, Sendable {
    public var kind: String
    public var agent: String
    public var person: String?
    public var spent: Double
    public var limit: Double
    public var month: String
    public var perRun: Double?
    public var raiseTo: Double?
    public var addTo: Double?
    public var at: String?

    public var isPerson: Bool { kind == "person" }
    /// The owner's bots together at their monthly cap (limits.rs bots_card).
    public var isBots: Bool { kind == "bots" }
}

/// A limit card's values: all the words, and which answer is drawn how.
public struct LimitCardModel: Equatable, Sendable {
    public enum Tone: Equatable, Sendable { case ink, pair, plain }
    public struct Option: Equatable, Sendable, Identifiable {
        public var id: String
        public var label: String
        public var tone: Tone
    }
    public var heading: String
    public var title: String
    public var left: String
    public var right: String
    public var fullness: Double
    public var options: [Option]
    public var footer: String?

    /// nil when the entry isn't a limit card. `board`: shown on Board, away from its chat.
    public init?(_ entry: Entry, personName: String? = nil, board: Bool = false) {
        guard let l = entry.data.limit else { return nil }
        let d = entry.data
        options = (d.options ?? []).enumerated().map { i, o in
            Option(id: o.optionId, label: o.name, tone: o.optionId == "stop" || o.optionId == "leave" ? .plain : i == 0 ? .ink : .pair)
        }
        right = Spend.of(l.spent, l.limit, cents: true)
        fullness = Spend.fullness(l.spent, l.limit)
        if l.isBots {
            heading = "Your bots · monthly cap"
            title = d.title ?? "Your bots reached their cap."
            left = "\(l.month), all agents, at API prices"
            footer = nil
        } else if l.isPerson {
            let name = personName ?? d.personName ?? l.person ?? "Someone"
            heading = "\(name) · limit on \(l.agent)"
            title = d.title ?? "\(name) reached their limit."
            left = "\(l.month) at API prices"
            footer = nil
        } else {
            heading = "\(l.agent) · Monthly limit"
            title = board ? Spend.question(entry) ?? "" : d.title ?? "\(l.agent) has used its \(Spend.money(l.limit)) for \(l.month)."
            left = l.perRun.map { "About $\(max(1, Int($0.rounded()))) a run" } ?? "\(l.month) at API prices"
            footer = d.status == "pending" && !board ? "Your message is kept. Nothing runs until you answer." : nil
        }
    }
}

/// Money and the words around it.
public enum Spend {
    public static let limitPresets: [Double] = [100, 200, 300, 500]
    public static let budgetPresets: [Double] = [10, 20, 50]
    /// The rows Usage shows before "N more".
    public static let shownAgents = 6

    /// "$6,667" from $10 up, "$2.92" below, or with `cents` always ("$289.01").
    public static func money(_ x: Double, cents: Bool = false) -> String {
        let f = NumberFormatter()
        f.locale = Locale(identifier: "en_US")
        f.numberStyle = .decimal
        if !cents && abs(x) >= 10 {
            f.maximumFractionDigits = 0
            return "$" + (f.string(from: NSNumber(value: x.rounded())) ?? "\(Int(x.rounded()))")
        }
        f.minimumFractionDigits = 2
        f.maximumFractionDigits = 2
        return "$" + (f.string(from: NSNumber(value: x)) ?? String(format: "%.2f", x))
    }

    public static func of(_ spent: Double, _ limit: Double, cents: Bool = false) -> String {
        "\(money(spent, cents: cents)) of \(money(limit))"
    }

    /// How full a limit is, 0…1; 0 with no limit.
    public static func fullness(_ spent: Double, _ limit: Double?) -> Double {
        guard let limit, limit > 0 else { return 0 }
        return min(1, max(0, spent / limit))
    }

    public static func runs(_ runs: Int, perRun: Double) -> String {
        "\(runs) \(runs == 1 ? "run" : "runs") · \(money(perRun)) a run"
    }

    /// Today, Week and this month's name.
    public static func periods(now: Date = .now) -> [(SpendPeriod, String)] {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US")
        f.dateFormat = "LLLL"
        return [(.today, "Today"), (.week, "Week"), (.month, f.string(from: now))]
    }

    private static let months = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"]

    /// "Oct 24" from "2026-10-24" or "2026-10-24 00:04:08".
    public static func shortDay(_ day: String) -> String {
        let parts = day.prefix(10).split(separator: "-")
        guard parts.count == 3, let m = Int(parts[1]), let d = Int(parts[2]), (1...12).contains(m) else { return day }
        return "\(months[m - 1]) \(d)"
    }

    /// The first of next month, for "starts again Nov 1".
    public static func nextMonthStart(now: Date = .now, calendar: Calendar = .current) -> String {
        "\(months[calendar.component(.month, from: now) % 12]) 1"
    }

    private static func isoDay(_ d: Date, _ calendar: Calendar) -> String {
        let c = calendar.dateComponents([.year, .month, .day], from: d)
        return String(format: "%04d-%02d-%02d", c.year ?? 0, c.month ?? 0, c.day ?? 0)
    }

    /// The limit sheet's forecast: "At about $72 a day, $300 runs out today and $500 lasts to about Oct 29."
    /// `chosen` nil (no limit) says the pace alone. Days come from the ledger's own forecast.
    public static func forecast(_ f: SpendForecast?, spent: Double, chosen: Double?, now: Date = .now, calendar: Calendar = .current) -> String {
        guard let f, f.perDay > 0 else {
            return chosen.map { "\(money($0)) a month; \(money(spent)) used so far." } ?? "No limit: runs never wait for one."
        }
        let pace = "At about \(money(f.perDay)) a day"
        guard let chosen else { return "\(pace), no limit: runs never wait for one." }
        let today = isoDay(now, calendar)
        let tomorrow = isoDay(calendar.date(byAdding: .day, value: 1, to: now) ?? now, calendar)
        func words(_ day: String) -> String { day <= today ? "today" : day == tomorrow ? "tomorrow" : "about \(shortDay(day))" }
        func say(_ amount: Double) -> String {
            if spent >= amount { return "\(money(amount)) is already used" }
            let key = String(Int(amount))
            if let day = f.reach[key] ?? nil { return "\(money(amount)) runs out \(words(day))" }
            return "\(money(amount)) lasts the month"
        }
        let lasts = spent < chosen && (f.reach[String(Int(chosen))] ?? nil) == nil
        guard !lasts, let next = limitPresets.first(where: { $0 > chosen }) else { return "\(pace), \(say(chosen))." }
        let after = say(next).replacingOccurrences(of: " runs out ", with: " lasts to ")
            .replacingOccurrences(of: "lasts to today", with: "runs out today")
            .replacingOccurrences(of: "lasts to tomorrow", with: "runs out tomorrow")
        return "\(pace), \(say(chosen)) and \(after)."
    }

    /// The agent sheet's forecast: "About $72 a day since Oct 23, so it reaches $300 today." Without a limit, the
    /// ledger's own sentence.
    public static func agentForecast(_ v: AgentUsage, now: Date = .now, calendar: Calendar = .current) -> String? {
        guard let f = v.forecast else { return nil }
        guard let limit = v.limit else { return f.text.isEmpty ? nil : f.text }
        let pace = "About \(money(f.perDay)) a day since \(shortDay(f.since))"
        if v.month >= limit { return "\(pace); it has used its \(money(limit))." }
        guard let day = f.reach[String(Int(limit))] ?? nil else { return "\(pace), so \(money(limit)) lasts the month." }
        return "\(pace), so it reaches \(money(limit)) \(day <= isoDay(now, calendar) ? "today" : "about \(shortDay(day))")."
    }

    /// "Opus 5.5 · high" from the model most of the cost ran on.
    public static func modelLine(_ models: [ModelSpend]) -> String {
        guard let top = models.max(by: { $0.cost < $1.cost }) else { return "—" }
        var name = top.model
        let parts = top.model.split(separator: "-")
        if parts.count >= 4, parts[0] == "claude" { name = "\(parts[1].prefix(1).uppercased())\(parts[1].dropFirst()) \(parts[2]).\(parts[3])" }
        return top.effort.map { "\(name) · \($0)" } ?? name
    }

    /// Board's line for a waiting limit card.
    public static func question(_ entry: Entry) -> String? {
        guard let l = entry.data.limit else { return nil }
        if l.isBots { return "Your bots reached their \(money(l.limit)) for \(l.month)." }
        if !l.isPerson { return "\(l.agent) used its \(money(l.limit)) for \(l.month). One run is waiting." }
        return "\(entry.data.personName ?? l.person ?? "Someone") reached their \(money(l.limit)) for \(l.month)."
    }

    /// The answered card's one line.
    public static func outcome(_ entry: Entry) -> String {
        let d = entry.data
        if d.status == "cancelled" { return "Closed" }
        let chosen = d.options?.first { $0.optionId == d.selected }
        switch d.selected {
        case "allow": return "Allowed this run"
        case "stop": return "Stopped · your message is kept"
        case "leave": return "Left as it is"
        case "raise", "add":
            guard let name = chosen?.name else { return "Raised" }
            if name.hasPrefix("Raise") { return "Raised" + name.dropFirst(5) }
            if name.hasPrefix("Add") { return "Added" + name.dropFirst(3) }
            return name
        default: return chosen?.name ?? "Answered"
        }
    }
}
