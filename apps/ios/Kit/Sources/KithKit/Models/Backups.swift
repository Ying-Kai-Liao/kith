import Foundation

// The backup chain: the accounts and providers Kith moves a bot's turns to when its own account runs out of usage,
// as the host's `backups` answers (serve/host/src/api/backups.rs), and above it each agent's main account, the one its
// turns start on. This app only shows them; they are chosen on the computer, where logging in also happens. Decoded
// leniently: an entry of an odd shape drops out alone.

public struct BackupEntry: Decodable, Hashable, Sendable, Identifiable {
    /// `claude`, `claude:work`, `claude-api:main`, `codex`, `codex:deep`.
    public var id: String
    public var ready: Bool
    /// Why it isn't ready.
    public var why: String?
    /// The one command that makes it ready, run in the computer's terminal.
    public var fix: String?
    /// While it is out of usage: when it is back.
    public var outUntil: Date?
    /// `rate_limit` (the provider refused a turn) or `usage_95` (switched early).
    public var reason: String?

    public init(id: String, ready: Bool = true, why: String? = nil, fix: String? = nil, outUntil: Date? = nil, reason: String? = nil) {
        self.id = id
        self.ready = ready
        self.why = why
        self.fix = fix
        self.outUntil = outUntil
        self.reason = reason
    }

    private enum CodingKeys: String, CodingKey { case id, ready, why, fix, outUntil, reason }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        ready = (try? c.decode(Bool.self, forKey: .ready)) ?? false
        why = try? c.decodeIfPresent(String.self, forKey: .why)
        fix = try? c.decodeIfPresent(String.self, forKey: .fix)
        outUntil = (try? c.decodeIfPresent(Double.self, forKey: .outUntil)).map { Date(timeIntervalSince1970: $0 / 1000) }
        reason = try? c.decodeIfPresent(String.self, forKey: .reason)
    }

    public func isOut(now: Date = Date()) -> Bool { outUntil.map { $0 > now } ?? false }

    /// What the user needs to know: ready, out until a time, or why it can't run.
    public func state(now: Date = Date()) -> String {
        if let until = outUntil, until > now { return "out of usage, " + Self.backAt(until, now: now) }
        if !ready { return why ?? "not ready" }
        return "ready"
    }

    /// "back at 3:40 PM", with the day when it isn't today.
    static func backAt(_ date: Date, now: Date) -> String {
        let sameDay = Calendar.current.isDate(date, inSameDayAs: now)
        let text = sameDay
            ? date.formatted(date: .omitted, time: .shortened)
            : date.formatted(.dateTime.month(.abbreviated).day().hour().minute())
        return "back at \(text)"
    }
}

/// The account an agent's turns start on (chain entry 0); its stored session belongs to it.
public struct MainAccount: Decodable, Hashable, Sendable {
    /// An entry id of the agent's harness: `claude`, `claude:work`, `claude-api:main`, `codex`, `codex:deep`.
    public var id: String
    /// `agent` (its own choice), `global` (the computer's) or `default` (the harness's own login).
    public var source: String
    public var ready: Bool
    public var why: String?
    public var fix: String?

    public init(id: String, source: String = "default", ready: Bool = true, why: String? = nil, fix: String? = nil) {
        self.id = id
        self.source = source
        self.ready = ready
        self.why = why
        self.fix = fix
    }

    private enum CodingKeys: String, CodingKey { case id, source, ready, why, fix }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        source = (try? c.decodeIfPresent(String.self, forKey: .source)) ?? "default"
        ready = (try? c.decodeIfPresent(Bool.self, forKey: .ready)) ?? true
        why = try? c.decodeIfPresent(String.self, forKey: .why)
        fix = try? c.decodeIfPresent(String.self, forKey: .fix)
    }

    /// Ready, or why it can't run.
    public var state: String { ready ? "ready" : (why ?? "not ready") }
}

public struct Backups: Decodable, Sendable, Equatable {
    /// Entry ids, in order.
    public var global: [String]
    /// Only the agents with an order of their own.
    public var agents: [String: [String]]
    public var entries: [BackupEntry]
    /// The computer's main account; nil is each harness's own login (and a host from before main accounts).
    public var mainGlobal: String?
    /// Every agent's main account, whatever it comes from; empty from a host before main accounts.
    public var mains: [String: MainAccount]

    public init(global: [String] = [], agents: [String: [String]] = [:], entries: [BackupEntry] = [],
                mainGlobal: String? = nil, mains: [String: MainAccount] = [:]) {
        self.global = global
        self.agents = agents
        self.entries = entries
        self.mainGlobal = mainGlobal
        self.mains = mains
    }

    private enum CodingKeys: String, CodingKey { case global, agents, entries, main }
    private enum MainKeys: String, CodingKey { case global, agents }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        global = (try? c.decode([String].self, forKey: .global)) ?? []
        agents = (try? c.decode([String: [String]].self, forKey: .agents)) ?? [:]
        var out: [BackupEntry] = []
        if var items = try? c.nestedUnkeyedContainer(forKey: .entries) {
            while !items.isAtEnd {
                if let entry = try? items.decode(BackupEntry.self) { out.append(entry) } else { _ = try? items.decode(Skip.self) }
            }
        }
        entries = out
        let main = try? c.nestedContainer(keyedBy: MainKeys.self, forKey: .main)
        mainGlobal = (try? main?.decodeIfPresent(String.self, forKey: .global)) ?? nil
        var mains: [String: MainAccount] = [:]
        if let raw = (try? main?.decodeIfPresent([String: Lenient<MainAccount>].self, forKey: .agents)) ?? nil {
            for (name, value) in raw { if let account = value.value { mains[name] = account } }
        }
        self.mains = mains
    }

    /// The agents whose main account is their own choice, by name: the ones that don't start where the computer does.
    public var ownMains: [(agent: String, main: MainAccount)] {
        mains.filter { $0.value.source == "agent" }.sorted { $0.key < $1.key }.map { (agent: $0.key, main: $0.value) }
    }

    /// The entry for `id`; one the host didn't describe is shown as not ready.
    public func entry(_ id: String) -> BackupEntry {
        entries.first { $0.id == id } ?? BackupEntry(id: id, ready: false)
    }

    /// One line for the global order, e.g. "codex, claude:work".
    public var globalText: String { global.joined(separator: ", ") }
}

private struct Skip: Decodable { init(from decoder: Decoder) throws {} }

/// A value that drops out alone (nil) when it doesn't decode.
private struct Lenient<T: Decodable>: Decodable {
    let value: T?
    init(from decoder: Decoder) throws { value = try? T(from: decoder) }
}

/// "via codex" under a reply a backup account answered; nil for the agent's own account and for older messages.
public func backupViaLabel(_ answeredBy: String?) -> String? {
    guard let answeredBy, !answeredBy.isEmpty else { return nil }
    return "via \(answeredBy)"
}
