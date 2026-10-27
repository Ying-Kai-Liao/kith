import Foundation

// A kith bot's detail page: what `kith <agent> --info-json` prints, passed through the host (serve/docs/bot-info.md).
// Decoded leniently: unknown keys are ignored, every list may be missing, and a part of an odd shape drops out
// instead of failing the page.

/// `kithInfo`: `info` is nil for a non-kith bot, or with `error` when kith could not answer.
public struct KithInfoResponse: Decodable, Sendable {
    public var info: KithInfo?
    public var error: String?
    public var fetchedAt: Int64?

    private enum CodingKeys: String, CodingKey { case info, error, fetchedAt }

    public init(info: KithInfo? = nil, error: String? = nil, fetchedAt: Int64? = nil) {
        self.info = info
        self.error = error
        self.fetchedAt = fetchedAt
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        info = c.value(.info)
        error = c.value(.error)
        fetchedAt = c.value(.fetchedAt)
    }
}

/// A time kith or the host sent: an ISO 8601 string (schedules, projects, memory files) or epoch milliseconds.
public struct KithDate: Decodable, Hashable, Sendable {
    public var date: Date

    public init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if let ms = try? c.decode(Double.self) {
            date = Date(timeIntervalSince1970: ms / 1000)
        } else if let s = try? c.decode(String.self), let d = Self.parse(s) {
            date = d
        } else {
            throw DecodingError.dataCorruptedError(in: c, debugDescription: "not a time")
        }
    }

    static func parse(_ s: String) -> Date? {
        let plain = ISO8601DateFormatter()
        if let d = plain.date(from: s) { return d }
        let fractional = ISO8601DateFormatter()
        fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return fractional.date(from: s)
    }
}

private struct Skip: Decodable { init(from decoder: Decoder) throws {} }

private extension KeyedDecodingContainer {
    /// A list that is missing, or in a shape this app doesn't know, is empty; one bad element drops out alone.
    func list<T: Decodable>(_ key: Key) -> [T] {
        guard var items = try? nestedUnkeyedContainer(forKey: key) else { return [] }
        var out: [T] = []
        while !items.isAtEnd {
            if let item = try? items.decode(T.self) { out.append(item) } else { _ = try? items.decode(Skip.self) }
        }
        return out
    }

    /// A value that is missing, null or of another type is nil.
    func value<T: Decodable>(_ key: Key) -> T? { (try? decodeIfPresent(T.self, forKey: key)) ?? nil }
}

public struct KithInfo: Decodable, Sendable {
    public var version: Int?
    public var agent: String
    public var description: String?
    public var harness: String?
    public var model: String?
    public var effort: String?
    public var permissionMode: String?
    /// The model a voice call runs on, as this Mac chose it: a model name, `own`, or `default` (sonnet).
    public var callModel: String?
    /// What `default` means on this Mac: the global call model (`own` is the agent's own).
    public var callModelDefault: String?
    public var home: String?
    public var role: KithRole?
    public var package: KithPackage?
    public var skills: [KithSkill]
    public var tools: [String]
    public var mcpServers: [KithMCPServer]
    public var plugins: [String]
    public var projects: [KithProject]
    public var schedules: [KithSchedule]
    public var cost: KithCost?
    public var memory: KithMemory?
    /// Parts kith could not read: key → reason.
    public var errors: [String: String]

    private enum CodingKeys: String, CodingKey {
        case version, agent, description, harness, model, effort, home, role, package, skills, tools, plugins, projects, schedules, cost, memory, errors
        case permissionMode = "permission_mode"
        case callModel = "call_model"
        case callModelDefault = "call_model_default"
        case mcpServers = "mcp_servers"
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        version = c.value(.version)
        agent = c.value(.agent) ?? ""
        description = c.value(.description)
        harness = c.value(.harness)
        model = c.value(.model)
        effort = c.value(.effort)
        permissionMode = c.value(.permissionMode)
        callModel = c.value(.callModel)
        callModelDefault = c.value(.callModelDefault)
        home = c.value(.home)
        role = c.value(.role)
        package = c.value(.package)
        skills = c.list(.skills)
        tools = c.list(.tools)
        mcpServers = c.list(.mcpServers)
        plugins = c.list(.plugins)
        projects = c.list(.projects)
        schedules = c.list(.schedules)
        cost = c.value(.cost)
        memory = c.value(.memory)
        errors = c.value(.errors) ?? [:]
    }
}

public struct KithRole: Decodable, Hashable, Sendable {
    public var path: String?
    public var intro: String
    public var sections: [KithRoleSection]

    private enum CodingKeys: String, CodingKey { case path, intro, sections }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        path = c.value(.path)
        intro = c.value(.intro) ?? ""
        sections = c.list(.sections)
    }

    /// The whole role prompt as one markdown text: the intro, then every section under its title. Nil when there is none.
    public var prompt: String? {
        let parts = [intro.trimmingCharacters(in: .whitespacesAndNewlines)]
            + sections.map { "## \($0.title)\n\n" + $0.text.trimmingCharacters(in: .whitespacesAndNewlines) }
        let text = parts.filter { !$0.isEmpty }.joined(separator: "\n\n")
        return text.isEmpty ? nil : text
    }
}

public struct KithRoleSection: Decodable, Hashable, Sendable, Identifiable {
    public var title: String
    public var text: String
    public var id: String { title }
    /// The section that says what the bot leaves to others; the page shows it as its own card.
    public var isNotYours: Bool { title.trimmingCharacters(in: .whitespaces).caseInsensitiveCompare("Not yours") == .orderedSame }

    private enum CodingKeys: String, CodingKey { case title, text }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        title = c.value(.title) ?? ""
        text = c.value(.text) ?? ""
    }
}

public struct KithPackage: Decodable, Hashable, Sendable {
    /// A fingerprint hash such as `393def5f55`, not a semver number.
    public var version: String?
    public var remote: String?
    public var dirty: Bool
    public var unpushed: Bool
    public var pinned: String?

    private enum CodingKeys: String, CodingKey { case version, remote, dirty, unpushed, pinned }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        version = c.value(.version)
        remote = c.value(.remote)
        dirty = c.value(.dirty) ?? false
        unpushed = c.value(.unpushed) ?? false
        pinned = c.value(.pinned)
    }

    /// The chip: `v` and the first 7 characters.
    public var chip: String? { version.map { "v" + $0.prefix(7) } }
}

public struct KithSkill: Decodable, Hashable, Sendable, Identifiable {
    public var name: String
    public var description: String
    /// `package` | `learned` | `global`.
    public var source: String
    public var id: String { source + "/" + name }

    private enum CodingKeys: String, CodingKey { case name, description, source }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        name = try c.decode(String.self, forKey: .name)
        description = c.value(.description) ?? ""
        source = c.value(.source) ?? "package"
    }
}

public struct KithMCPServer: Decodable, Hashable, Sendable, Identifiable {
    public var name: String
    public var kind: String?
    public var command: String?
    public var url: String?
    public var id: String { name }

    private enum CodingKeys: String, CodingKey { case name, kind, command, url }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        name = try c.decode(String.self, forKey: .name)
        kind = c.value(.kind)
        command = c.value(.command)
        url = c.value(.url)
    }

    /// The command it runs, or the host it talks to.
    public var target: String? {
        if let command, !command.isEmpty { return command }
        guard let url, !url.isEmpty else { return nil }
        return URL(string: url)?.host ?? url
    }
}

public struct KithProject: Decodable, Hashable, Sendable, Identifiable {
    public var id: String
    public var files: Int
    public var modified: KithDate?

    private enum CodingKeys: String, CodingKey { case id, files, modified }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        files = c.value(.files) ?? 0
        modified = c.value(.modified)
    }
}

public struct KithSchedule: Decodable, Hashable, Sendable, Identifiable {
    public var id: String
    public var cron: String
    public var task: String
    public var paused: Bool
    public var lastRun: KithDate?
    /// `running`, `done` or nil.
    public var lastStatus: String?
    public var nextRun: KithDate?

    private enum CodingKeys: String, CodingKey {
        case id, cron, task, paused
        case lastRun = "last_run", lastStatus = "last_status", nextRun = "next_run"
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        cron = c.value(.cron) ?? ""
        task = c.value(.task) ?? ""
        paused = c.value(.paused) ?? false
        lastRun = c.value(.lastRun)
        lastStatus = c.value(.lastStatus)
        nextRun = c.value(.nextRun)
    }

    public var isRunning: Bool { lastStatus == "running" }

    /// "Daily at 09:00", "Weekdays at 09:00", "Every 15 minutes", "Hourly"; the raw cron when it is anything else.
    public var plainCron: String { KithSchedule.describe(cron) }

    static func describe(_ cron: String) -> String {
        let f = cron.split(whereSeparator: \.isWhitespace).map(String.init)
        guard f.count == 5 else { return cron }
        let (minute, hour, dom, month, dow) = (f[0], f[1], f[2], f[3], f[4])
        guard dom == "*", month == "*" else { return cron }
        if dow == "*" {
            if hour == "*", minute == "0" { return "Hourly" }
            if hour == "*", minute == "*" { return "Every minute" }
            if hour == "*", minute.hasPrefix("*/"), let n = Int(minute.dropFirst(2)), n > 0 {
                return n == 1 ? "Every minute" : "Every \(n) minutes"
            }
        }
        guard let h = Int(hour), let m = Int(minute), (0..<24).contains(h), (0..<60).contains(m) else { return cron }
        let time = String(format: "%02d:%02d", h, m)
        if dow == "*" { return "Daily at " + time }
        if ["1-5", "MON-FRI", "mon-fri"].contains(dow) { return "Weekdays at " + time }
        return cron
    }
}

public struct KithCost: Decodable, Hashable, Sendable {
    public var currency: String
    public var today: Double
    public var week: Double
    public var month: Double
    public var all: Double
    public var requestsMonth: Int
    public var byModelMonth: [KithModelCost]

    private enum CodingKeys: String, CodingKey {
        case currency, today, week, month, all
        case requestsMonth = "requests_month", byModelMonth = "by_model_month"
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        currency = c.value(.currency) ?? "USD"
        today = c.value(.today) ?? 0
        week = c.value(.week) ?? 0
        month = c.value(.month) ?? 0
        all = c.value(.all) ?? 0
        requestsMonth = c.value(.requestsMonth) ?? 0
        byModelMonth = c.list(.byModelMonth)
    }

    /// The three biggest models this month, with their share of the month.
    public var topModels: [(model: String, usd: Double, share: Double)] {
        let total = byModelMonth.reduce(0) { $0 + $1.usd }
        return byModelMonth.sorted { $0.usd > $1.usd }.prefix(3).map { ($0.model, $0.usd, total > 0 ? $0.usd / total : 0) }
    }

    /// "$0.12", "<$0.01" for a tiny amount, "$0.00" for none.
    public static func format(_ usd: Double) -> String {
        if usd <= 0 { return "$0.00" }
        if usd < 0.005 { return "<$0.01" }
        return String(format: "$%.2f", usd)
    }
}

public struct KithModelCost: Decodable, Hashable, Sendable {
    public var model: String
    public var usd: Double

    private enum CodingKeys: String, CodingKey { case model, usd }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        model = c.value(.model) ?? ""
        usd = c.value(.usd) ?? 0
    }
}

public struct KithMemory: Decodable, Hashable, Sendable {
    public var dir: String?
    public var files: [KithMemoryEntry]
    public var totalBytes: Int

    private enum CodingKeys: String, CodingKey { case dir, files, totalBytes = "total_bytes" }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        dir = c.value(.dir)
        files = c.list(.files)
        totalBytes = c.value(.totalBytes) ?? 0
    }

    /// MEMORY.md first, then the newest.
    public var ordered: [KithMemoryEntry] {
        files.sorted {
            if $0.isIndex != $1.isIndex { return $0.isIndex }
            return ($0.modified?.date ?? .distantPast) > ($1.modified?.date ?? .distantPast)
        }
    }
}

public struct KithMemoryEntry: Decodable, Hashable, Sendable, Identifiable {
    public var path: String
    public var bytes: Int
    public var modified: KithDate?
    public var id: String { path }
    public var isIndex: Bool { path == "MEMORY.md" }

    private enum CodingKeys: String, CodingKey { case path, bytes, modified }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        path = try c.decode(String.self, forKey: .path)
        bytes = c.value(.bytes) ?? 0
        modified = c.value(.modified)
    }
}

/// `kithMemoryFile`.
public struct KithMemoryFile: Decodable, Hashable, Sendable {
    public var path: String
    public var text: String
    public var bytes: Int
    public var modified: KithDate?
    /// `text` is only the first 512 KB.
    public var truncated: Bool

    private enum CodingKeys: String, CodingKey { case path, text, bytes, modified, truncated }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        path = c.value(.path) ?? ""
        text = c.value(.text) ?? ""
        bytes = c.value(.bytes) ?? 0
        modified = c.value(.modified)
        truncated = c.value(.truncated) ?? false
    }
}

/// `kithMemorySearch`: `error` says why `hits` is empty when kith failed.
public struct KithSearchResult: Decodable, Sendable {
    public var hits: [KithSearchHit]
    public var error: String?

    private enum CodingKeys: String, CodingKey { case hits, error }

    public init(hits: [KithSearchHit] = [], error: String? = nil) {
        self.hits = hits
        self.error = error
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        hits = c.list(.hits)
        error = c.value(.error)
    }
}

public struct KithSearchHit: Decodable, Hashable, Sendable, Identifiable {
    public var path: String
    public var line: Int?
    public var snippet: String
    /// Only the host's `openable` hits can be read with `kithMemoryFile`.
    public var openable: Bool
    /// The agent whose file it is, for hits outside this bot's memory.
    public var agent: String?
    public var id: String { "\(path):\(line ?? 0):\(snippet)" }

    private enum CodingKeys: String, CodingKey { case path, line, snippet, openable, agent }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        path = try c.decode(String.self, forKey: .path)
        line = c.value(.line)
        snippet = c.value(.snippet) ?? ""
        openable = c.value(.openable) ?? false
        agent = c.value(.agent)
    }
}

/// One "always" answer to an escape card that kith-auth keeps (`kithAccessGrants`, serve/docs/bot-info.md): `person` may
/// `kind` (read, write, domain) `target` beyond the box in every run, until it is removed. `since` is nil for one
/// kith-auth kept no time for.
public struct AccessGrant: Decodable, Hashable, Sendable, Identifiable {
    public var person: String
    public var name: String
    public var kind: String
    public var target: String
    public var since: Double?
    public var id: String { "\(person)\n\(kind)\n\(target)" }

    public init(person: String, name: String = "", kind: String, target: String, since: Double? = nil) {
        self.person = person
        self.name = name
        self.kind = kind
        self.target = target
        self.since = since
    }

    /// What it lets them do: "Read ~/docs", "Read and write ~/notes", "Connect to pypi.org".
    public var text: String {
        switch kind {
        case "domain": "Connect to \(target)"
        case "write": "Read and write \(target)"
        default: "Read \(target)"
        }
    }

    /// Who holds it and since when: "Sam · since 3 Oct 2026". The everyone row (`*`) is "Everyone".
    public var holder: String {
        let who = person == "*" ? "Everyone" : (name.isEmpty ? person : name)
        guard let since else { return who }
        let date = Date(timeIntervalSince1970: since / 1000).formatted(.dateTime.day().month(.abbreviated).year())
        return "\(who) · since \(date)"
    }
}

/// The host's `kithAccessGrants` and `kithRemoveAccessGrant` answer; a grant this app can't read is left out.
public struct AccessGrants: Decodable, Sendable {
    public var grants: [AccessGrant]

    private enum CodingKeys: String, CodingKey { case grants }
    private struct Raw: Decodable { var person: String; var name: String?; var kind: String; var target: String; var since: Double? }

    public init(grants: [AccessGrant] = []) { self.grants = grants }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let raw: [Raw] = c.list(.grants)
        grants = raw.filter { ["read", "write", "domain"].contains($0.kind) && !$0.person.isEmpty && !$0.target.isEmpty }
            .map { AccessGrant(person: $0.person, name: $0.name ?? "", kind: $0.kind, target: $0.target, since: $0.since) }
    }
}
