import Foundation

/// The owner's peek at a handoff (a `kith ask`): host `delegationPeek` / `delegationsPeek`
/// (serve/host/src/api/delegations.rs). Owner only. A state or field this app doesn't know still reads.
public struct HandoffPeek: Decodable, Equatable, Sendable, Identifiable {
    public enum State: String, Sendable { case sent, working, needsYou, done, stopped, unknown }

    public struct Party: Decodable, Equatable, Sendable {
        public var agent: String?
        public var person: String?
    }

    /// The worker's thread on this Mac; a nil `threadId` is the bot's main chat.
    public struct Thread: Decodable, Equatable, Sendable {
        public var botId: String
        public var threadId: String?
    }

    /// The newest tool row or text of the worker, frozen once it is done.
    public struct Live: Decodable, Equatable, Sendable {
        public var kind: String
        public var text: String
        public var isTool: Bool { kind == "tool" }
    }

    public struct Limits: Decodable, Equatable, Sendable {
        public var costMicroUsd: Int64?
        public var capMicroUsd: Int64?
        public var chainCostMicroUsd: Int64?
        public var hop: Int?
        public var hopMax: Int?
        public var crossHops: Int?
        public var crossMax: Int?
        public var elapsedMs: Int64?

        public init(costMicroUsd: Int64? = nil, capMicroUsd: Int64? = nil, chainCostMicroUsd: Int64? = nil, hop: Int? = nil,
                    hopMax: Int? = nil, crossHops: Int? = nil, crossMax: Int? = nil, elapsedMs: Int64? = nil) {
            self.costMicroUsd = costMicroUsd
            self.capMicroUsd = capMicroUsd
            self.chainCostMicroUsd = chainCostMicroUsd
            self.hop = hop
            self.hopMax = hopMax
            self.crossHops = crossHops
            self.crossMax = crossMax
            self.elapsedMs = elapsedMs
        }
    }

    public var id: String
    public var state: State
    public var brief: String?
    public var from: Party?
    public var to: String
    public var crossed: Bool
    public var thread: Thread?
    public var live: Live?
    public var limits: Limits
    public var startedMs: Int64
    public var answeredMs: Int64?
    public var cappedMs: Int64?

    private enum Keys: String, CodingKey {
        case id, state, brief, from, to, crossed, thread, live, limits, startedMs, answeredMs, cappedMs
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        id = try c.decode(String.self, forKey: .id)
        let word = try? c.decode(String.self, forKey: .state)
        state = word.flatMap(State.init(rawValue:)) ?? .unknown
        brief = (try? c.decode(String.self, forKey: .brief)).flatMap { $0.isEmpty ? nil : $0 }
        from = try? c.decode(Party.self, forKey: .from)
        to = (try? c.decode(String.self, forKey: .to)) ?? ""
        crossed = (try? c.decode(Bool.self, forKey: .crossed)) ?? false
        thread = try? c.decode(Thread.self, forKey: .thread)
        live = (try? c.decode(Live.self, forKey: .live)).flatMap { $0.text.isEmpty ? nil : $0 }
        limits = (try? c.decode(Limits.self, forKey: .limits)) ?? Limits()
        startedMs = (try? c.decode(Int64.self, forKey: .startedMs)) ?? 0
        answeredMs = try? c.decode(Int64.self, forKey: .answeredMs)
        cappedMs = try? c.decode(Int64.self, forKey: .cappedMs)
    }

    // MARK: what the sheet says

    public var isOpen: Bool { state == .sent || state == .working || state == .needsYou }

    public var stateLabel: String {
        switch state {
        case .sent: "Sent"
        case .working: "Working"
        case .needsYou: "Needs you"
        case .done: "Done"
        case .stopped: "You stopped it"
        case .unknown: "No record"
        }
    }

    public var title: String {
        if let name = from?.agent ?? from?.person { return "\(name) → \(to)" }
        return "To \(to)"
    }

    /// Why Join is off, or nil when it is on. A finished handoff's thread still opens.
    public var joinBlocked: String? {
        if thread != nil { return nil }
        if crossed { return "It runs on another Mac" }
        return state == .sent ? "It has not started yet" : "Its thread isn't on this Mac"
    }

    /// Why Stop is off, or nil when it is on. A finished or stopped handoff has nothing to stop.
    public var stopBlocked: String? {
        guard isOpen else { return state == .unknown ? "No record of it" : "It is not running" }
        return thread != nil ? nil : joinBlocked
    }

    /// The box to the worker writes into its thread: open work with a thread on this Mac.
    public var sendBlocked: String? { isOpen ? joinBlocked : "It is not running" }

    // MARK: limits

    public struct LimitPart: Equatable, Sendable {
        public enum Kind: Sendable { case budget, hops, crossings, elapsed }
        public var kind: Kind
        public var label: String
        /// 0...1 of the limit used; nil for a clock, which has no limit.
        public var fraction: Double?
        /// The one closest to its limit.
        public var watch = false
    }

    private static func dollars(_ micro: Int64) -> String {
        let d = Double(micro) / 1_000_000
        return d == d.rounded() ? "$\(Int(d))" : String(format: "$%.2f", d)
    }

    static func elapsedLabel(_ ms: Int64) -> String {
        let s = ms / 1000
        if s < 60 { return "\(s) s" }
        let m = s / 60
        return m < 60 ? "\(m) min" : "\(m / 60) h \(m % 60) min"
    }

    /// The limits line: budget bar and "$0.70 / $3", hops, crossings, elapsed; whichever is closest to its limit is flagged.
    public var limitParts: [LimitPart] {
        var parts: [LimitPart] = []
        let l = limits
        if let spent = l.chainCostMicroUsd ?? l.costMicroUsd {
            if let cap = l.capMicroUsd, cap > 0 {
                parts.append(.init(kind: .budget, label: "\(Self.dollars(spent)) / \(Self.dollars(cap))", fraction: min(1, Double(spent) / Double(cap))))
            } else {
                parts.append(.init(kind: .budget, label: Self.dollars(spent), fraction: nil))
            }
        }
        if let hop = l.hop {
            let max = l.hopMax ?? 8
            parts.append(.init(kind: .hops, label: "hop \(hop) / \(max)", fraction: min(1, Double(hop) / Double(max))))
        }
        if let cross = l.crossHops, cross > 0 {
            let max = l.crossMax ?? 2
            parts.append(.init(kind: .crossings, label: "\(cross) / \(max) crossings", fraction: min(1, Double(cross) / Double(max))))
        }
        if let ms = l.elapsedMs { parts.append(.init(kind: .elapsed, label: Self.elapsedLabel(ms), fraction: nil)) }
        var best: Int?
        for (i, p) in parts.enumerated() {
            if let f = p.fraction, f > (best.flatMap { parts[$0].fraction } ?? 0) { best = i }
        }
        if let best { parts[best].watch = true }
        return parts
    }

    // MARK: the row that led here

    /// The ask a tool row belongs to. `kith ask --bg` prints "started echo in the background as bg-…", a run waiting on
    /// a card prints its `id`, and `--wait` / `--to` name it on the command line. A plain foreground ask prints no id
    /// until it is done, so its row is not tappable; neither is a row from before the delegation record.
    public static func askId(command: String?, output: String?) -> String? {
        guard let command, command.range(of: #"\bkith(\s+|-)ask\b"#, options: .regularExpression) != nil else { return nil }
        let pattern = #"\b(?:bg|ask)-\d{14}-[0-9a-f]{6}\b"#
        for text in [command, output ?? ""] {
            if let r = text.range(of: pattern, options: .regularExpression) { return String(text[r]) }
        }
        return nil
    }
}

public extension HostClient {
    /// One handoff for the owner's peek sheet. Owner only; a computer before the peek answers an error.
    func peekHandoff(_ id: String) async throws -> HandoffPeek {
        struct Body: Encodable { var id: String }
        struct Res: Decodable { var peek: HandoffPeek }
        let res: Res = try await call("delegationPeek", Body(id: id), timeout: 25)
        return res.peek
    }

    /// The handoffs still going or waiting on the owner.
    func openHandoffs() async throws -> [HandoffPeek] {
        struct Res: Decodable { var peeks: [LossyPeek] }
        let res: Res = try await call("delegationsPeek", Empty(), timeout: 25)
        return res.peeks.compactMap(\.peek)
    }

    /// One peek that doesn't read is left out, not the whole list.
    private struct LossyPeek: Decodable {
        var peek: HandoffPeek?
        init(from decoder: Decoder) throws { peek = try? HandoffPeek(from: decoder) }
    }
}
