import Foundation

// The computer's managed account set (host `accounts*` methods, serve/host/src/api/accounts.rs; docs/accounts.md): every
// harness account the kernel knows, which are in the set and in what order, the policy, and each agent's view. Every
// write answers with the same read, fresh. Decoded leniently: an entry of an odd shape drops out alone, and a status
// this app doesn't know is kept as `.other`. Not the Kith user account (cloud sign-in): those are `AccountSession` etc.

public struct HarnessAccount: Decodable, Hashable, Sendable, Identifiable {
    public enum Status: Hashable, Sendable {
        case ready, out, signedOut, unsupported, invalid
        case other(String)

        init(_ raw: String) {
            switch raw {
            case "ready": self = .ready
            case "out": self = .out
            case "signed_out": self = .signedOut
            case "unsupported": self = .unsupported
            case "invalid": self = .invalid
            default: self = .other(raw)
            }
        }
    }

    public struct Window: Decodable, Hashable, Sendable {
        public var name: String
        public var percent: Double?
        public var resetsAt: Date?

        private enum CodingKeys: String, CodingKey { case name, percent, resetsAt }

        public init(name: String, percent: Double? = nil, resetsAt: Date? = nil) {
            self.name = name
            self.percent = percent
            self.resetsAt = resetsAt
        }

        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            name = (try? c.decode(String.self, forKey: .name)) ?? ""
            percent = try? c.decodeIfPresent(Double.self, forKey: .percent)
            resetsAt = (try? c.decodeIfPresent(Double.self, forKey: .resetsAt)).map { Date(timeIntervalSince1970: $0 / 1000) }
        }
    }

    public struct Pin: Decodable, Hashable, Sendable {
        public var agent: String
        public var only: Bool
    }

    /// `claude`, `claude:work`, `codex:deep`.
    public var id: String
    public var harness: String
    public var account: String
    public var kind: String?
    public var inSet: Bool
    /// Position in the global set (when in it).
    public var order: Int?
    public var status: Status
    public var ready: Bool
    public var why: String?
    /// The one command that makes it ready, run in the computer's terminal.
    public var fix: String?
    public var outUntil: Date?
    public var reason: String?
    public var windows: [Window]
    public var pins: [Pin]
    public var loggedIn: Bool

    private enum CodingKeys: String, CodingKey {
        case id, harness, account, kind, inSet, order, status, ready, why, fix, outUntil, reason, usage, pins, loggedIn
    }
    private enum UsageKeys: String, CodingKey { case windows }

    public init(id: String, harness: String = "", account: String = "", kind: String? = nil, inSet: Bool = false,
                order: Int? = nil, status: Status = .ready, ready: Bool = true, why: String? = nil, fix: String? = nil,
                outUntil: Date? = nil, reason: String? = nil, windows: [Window] = [], pins: [Pin] = [], loggedIn: Bool = true) {
        self.id = id
        self.harness = harness
        self.account = account
        self.kind = kind
        self.inSet = inSet
        self.order = order
        self.status = status
        self.ready = ready
        self.why = why
        self.fix = fix
        self.outUntil = outUntil
        self.reason = reason
        self.windows = windows
        self.pins = pins
        self.loggedIn = loggedIn
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        let parts = id.split(separator: ":", maxSplits: 1).map(String.init)
        harness = (try? c.decodeIfPresent(String.self, forKey: .harness)) ?? parts.first ?? id
        account = (try? c.decodeIfPresent(String.self, forKey: .account)) ?? (parts.count > 1 ? parts[1] : "")
        kind = try? c.decodeIfPresent(String.self, forKey: .kind)
        inSet = (try? c.decodeIfPresent(Bool.self, forKey: .inSet)) ?? false
        order = try? c.decodeIfPresent(Int.self, forKey: .order)
        status = Status((try? c.decodeIfPresent(String.self, forKey: .status)) ?? "ready")
        ready = (try? c.decodeIfPresent(Bool.self, forKey: .ready)) ?? false
        why = try? c.decodeIfPresent(String.self, forKey: .why)
        fix = try? c.decodeIfPresent(String.self, forKey: .fix)
        outUntil = (try? c.decodeIfPresent(Double.self, forKey: .outUntil)).map { Date(timeIntervalSince1970: $0 / 1000) }
        reason = try? c.decodeIfPresent(String.self, forKey: .reason)
        let usage = try? c.nestedContainer(keyedBy: UsageKeys.self, forKey: .usage)
        windows = (try? usage?.decodeIfPresent([HarnessAccount.Window].self, forKey: .windows)) ?? []
        pins = (try? c.decodeIfPresent([Pin].self, forKey: .pins)) ?? []
        loggedIn = (try? c.decodeIfPresent(Bool.self, forKey: .loggedIn)) ?? true
    }

    /// What the user needs to know: ready, out until a time (and why), the fix for a signed-out one, or unsupported.
    public func state(now: Date = Date()) -> String {
        switch status {
        case .ready: return "ready"
        case .out:
            guard let until = outUntil else { return "out of usage" }
            let back = BackupEntry.backAt(until, now: now).replacingOccurrences(of: "back at", with: "out until")
            return reason.map { "\(back) (\(Self.reasonText($0)))" } ?? back
        case .signedOut: return "signed out"
        case .unsupported: return why ?? "unsupported"
        case .invalid: return why ?? "invalid"
        case .other(let raw): return why ?? raw
        }
    }

    static func reasonText(_ reason: String) -> String {
        switch reason {
        case "rate_limit": return "rate limit"
        case "usage_95": return "usage at 95%"
        default: return reason
        }
    }
}

public struct HarnessAgentAccounts: Decodable, Hashable, Sendable {
    public struct Pin: Decodable, Hashable, Sendable {
        public var id: String
        public var only: Bool
    }

    public struct Skipped: Decodable, Hashable, Sendable {
        public var id: String
        public var status: String?
        public var why: String?
        public var outUntil: Date?
        public var reason: String?

        private enum CodingKeys: String, CodingKey { case id, status, why, outUntil, reason }

        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            id = try c.decode(String.self, forKey: .id)
            status = try? c.decodeIfPresent(String.self, forKey: .status)
            why = try? c.decodeIfPresent(String.self, forKey: .why)
            outUntil = (try? c.decodeIfPresent(Double.self, forKey: .outUntil)).map { Date(timeIntervalSince1970: $0 / 1000) }
            reason = try? c.decodeIfPresent(String.self, forKey: .reason)
        }
    }

    public var harness: String
    public var pin: Pin?
    /// The agent's own list; nil follows the global set.
    public var own: [String]?
    /// Static order, pin first.
    public var order: [String]
    public var main: String?
    /// What a run would try now.
    public var candidates: [String]
    public var allOut: Bool
    public var crossHarness: Bool
    public var skipped: [Skipped]

    private enum CodingKeys: String, CodingKey {
        case harness, pin, own, order, main, candidates, allOut, crossHarness, skipped
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        harness = (try? c.decodeIfPresent(String.self, forKey: .harness)) ?? ""
        pin = try? c.decodeIfPresent(Pin.self, forKey: .pin)
        own = (try? c.decodeIfPresent([String].self, forKey: .own)) ?? nil
        order = (try? c.decodeIfPresent([String].self, forKey: .order)) ?? []
        main = try? c.decodeIfPresent(String.self, forKey: .main)
        candidates = (try? c.decodeIfPresent([String].self, forKey: .candidates)) ?? []
        allOut = (try? c.decodeIfPresent(Bool.self, forKey: .allOut)) ?? false
        crossHarness = (try? c.decodeIfPresent(Bool.self, forKey: .crossHarness)) ?? false
        var skipped: [Skipped] = []
        if var items = try? c.nestedUnkeyedContainer(forKey: .skipped) {
            while !items.isAtEnd {
                if let s = try? items.decode(Skipped.self) { skipped.append(s) } else { _ = try? items.decode(DropOne.self) }
            }
        }
        self.skipped = skipped
    }
}

public struct HarnessAccounts: Decodable, Sendable, Equatable {
    /// `fill` (use accounts in order until each is out) or `spread`.
    public var policy: String
    public var crossHarness: Bool
    public var accounts: [HarnessAccount]
    /// Only the agents the host could read; an agent missing here is unavailable.
    public var agents: [String: HarnessAgentAccounts]

    private enum CodingKeys: String, CodingKey { case policy, crossHarness, accounts, agents }

    public init(policy: String = "fill", crossHarness: Bool = false, accounts: [HarnessAccount] = [],
                agents: [String: HarnessAgentAccounts] = [:]) {
        self.policy = policy
        self.crossHarness = crossHarness
        self.accounts = accounts
        self.agents = agents
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        policy = (try? c.decodeIfPresent(String.self, forKey: .policy)) ?? "fill"
        crossHarness = (try? c.decodeIfPresent(Bool.self, forKey: .crossHarness)) ?? false
        var out: [HarnessAccount] = []
        if var items = try? c.nestedUnkeyedContainer(forKey: .accounts) {
            while !items.isAtEnd {
                if let a = try? items.decode(HarnessAccount.self) { out.append(a) } else { _ = try? items.decode(DropOne.self) }
            }
        }
        accounts = out
        var agents: [String: HarnessAgentAccounts] = [:]
        if let raw = (try? c.decodeIfPresent([String: LenientValue<HarnessAgentAccounts>].self, forKey: .agents)) ?? nil {
            for (name, v) in raw { if let a = v.value { agents[name] = a } }
        }
        self.agents = agents
    }

    public static func == (a: HarnessAccounts, b: HarnessAccounts) -> Bool {
        a.policy == b.policy && a.crossHarness == b.crossHarness && a.accounts.map(\.id) == b.accounts.map(\.id)
            && a.agents.mapValues(\.order) == b.agents.mapValues(\.order)
    }

    /// The accounts in the global set, in order.
    public var set: [HarnessAccount] {
        accounts.filter(\.inSet).sorted { ($0.order ?? .max) < ($1.order ?? .max) }
    }

    public var outsideSet: [HarnessAccount] { accounts.filter { !$0.inSet } }

    public func account(_ id: String) -> HarnessAccount {
        accounts.first { $0.id == id } ?? HarnessAccount(id: id, status: .invalid, ready: false, why: "not described")
    }

    public var agentNames: [String] { agents.keys.sorted() }
}

private struct DropOne: Decodable { init(from decoder: Decoder) throws {} }

private struct LenientValue<T: Decodable>: Decodable {
    let value: T?
    init(from decoder: Decoder) throws { value = try? T(from: decoder) }
}
