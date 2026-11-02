import Foundation

// An hq agent's memory as plain files (serve/docs/bot-info.md, "Memory files and tidy-ups"). Timestamps are
// milliseconds since 1970. Decoding is lenient: a field a newer host adds or an older one lacks never fails a screen.

/// One Markdown file in the agent's memory folder or the shared user layer.
public struct HqMemoryFileEntry: Decodable, Identifiable, Hashable, Sendable {
    public var name: String
    public var shared: Bool
    public var size: Int
    public var modified: Int64

    public var id: String { "\(shared ? "shared" : "agent"):\(name)" }

    private enum CodingKeys: String, CodingKey { case name, shared, size, modified }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        name = try c.decode(String.self, forKey: .name)
        shared = c.lenient(.shared) ?? false
        size = c.lenient(.size) ?? 0
        modified = c.lenient(.modified) ?? 0
    }

    public init(name: String, shared: Bool, size: Int = 0, modified: Int64 = 0) {
        self.name = name
        self.shared = shared
        self.size = size
        self.modified = modified
    }
}

/// `kithMemoryList`: the agent's files (`MEMORY.md` first) and the shared ones.
public struct HqMemoryList: Decodable, Hashable, Sendable {
    public var agent: [HqMemoryFileEntry]
    public var shared: [HqMemoryFileEntry]

    private enum CodingKeys: String, CodingKey { case agent, shared }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let own: [HqMemoryFileEntry] = c.lenient(.agent) ?? []
        // The host lists only files that exist; these two open ready to write, and Save creates them.
        let present = own.contains { $0.name == "MEMORY.md" } ? own : own + [HqMemoryFileEntry(name: "MEMORY.md", shared: false)]
        agent = present.sorted { ($0.name == "MEMORY.md" ? 0 : 1, $0.name) < ($1.name == "MEMORY.md" ? 0 : 1, $1.name) }
        let others: [HqMemoryFileEntry] = c.lenient(.shared) ?? []
        shared = others.contains { $0.name == "user.md" } ? others : [HqMemoryFileEntry(name: "user.md", shared: true)] + others
    }

    public init(agent: [HqMemoryFileEntry] = [], shared: [HqMemoryFileEntry] = []) {
        self.agent = agent
        self.shared = shared
    }
}

/// `kithMemoryRead`. A file that isn't made yet has empty text and `exists` false; Save creates it.
public struct HqMemoryText: Decodable, Hashable, Sendable {
    public var name: String
    public var shared: Bool
    public var text: String
    public var bytes: Int
    public var modified: Int64
    /// `text` is only the first 512 KB: show it, don't edit it.
    public var truncated: Bool
    public var exists: Bool

    private enum CodingKeys: String, CodingKey { case name, shared, text, bytes, modified, truncated, exists }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        name = c.lenient(.name) ?? ""
        shared = c.lenient(.shared) ?? false
        text = c.lenient(.text) ?? ""
        bytes = c.lenient(.bytes) ?? 0
        modified = c.lenient(.modified) ?? 0
        truncated = c.lenient(.truncated) ?? false
        exists = c.lenient(.exists) ?? true
    }
}

/// One saved version of a file.
public struct HqMemoryVersion: Decodable, Identifiable, Hashable, Sendable {
    public var id: Int64
    public var at: Int64
    /// kai · agent · tidy · unknown (a change made outside the hook). Older rows say `dream`; they read as `tidy`.
    public var author: String
    public var authorBot: String?
    /// edit · delete · restore · undo · tidy · baseline
    public var cause: String
    public var deleted: Bool
    public var bytes: Int
    public var lines: Int

    private enum CodingKeys: String, CodingKey { case id, at, author, authorBot, cause, deleted, bytes, lines }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(Int64.self, forKey: .id)
        at = c.lenient(.at) ?? 0
        author = Self.renamed(c.lenient(.author) ?? "unknown")
        authorBot = c.lenient(.authorBot)
        cause = Self.renamed(c.lenient(.cause) ?? "edit")
        deleted = c.lenient(.deleted) ?? false
        bytes = c.lenient(.bytes) ?? 0
        lines = c.lenient(.lines) ?? 0
    }

    private static func renamed(_ word: String) -> String { word == "dream" ? "tidy" : word }
}

/// `kithMemorySave` and `kithMemoryRestore`: the version made (nil when nothing changed) and what the checks noticed.
public struct HqMemoryWrite: Decodable, Hashable, Sendable {
    public var version: HqMemoryVersion?
    public var warnings: [String]

    private enum CodingKeys: String, CodingKey { case version, warnings }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        version = c.lenient(.version)
        warnings = c.lenient(.warnings) ?? []
    }
}

/// `kithMemoryHistory`, newest first.
public struct HqMemoryHistory: Decodable, Hashable, Sendable {
    public var versions: [HqMemoryVersion]

    private enum CodingKeys: String, CodingKey { case versions }

    public init(from decoder: Decoder) throws {
        versions = try decoder.container(keyedBy: CodingKeys.self).lenient(.versions) ?? []
    }
}

/// `kithMemoryDiff`: lines starting `- ` (gone), `+ ` (new), `  ` (context) or `…` (a gap).
public struct HqMemoryDiff: Decodable, Hashable, Sendable {
    public var diff: String

    private enum CodingKeys: String, CodingKey { case diff }

    public init(from decoder: Decoder) throws {
        diff = try decoder.container(keyedBy: CodingKeys.self).lenient(.diff) ?? ""
    }
}

/// One background tidy of the agent's files.
public struct HqTidyUp: Decodable, Identifiable, Hashable, Sendable {
    public struct Change: Decodable, Hashable, Sendable {
        /// added · archived · reviewed · noted · indexed
        public var kind: String
        public var file: String
        public var shared: Bool
        public var text: String
        /// The old line, for `reviewed`.
        public var was: String?

        private enum CodingKeys: String, CodingKey { case kind, file, shared, text, was }

        public init(from decoder: Decoder) throws {
            let c = try decoder.container(keyedBy: CodingKeys.self)
            kind = c.lenient(.kind) ?? ""
            file = c.lenient(.file) ?? ""
            shared = c.lenient(.shared) ?? false
            text = c.lenient(.text) ?? ""
            was = c.lenient(.was)
        }
    }

    public var id: Int64
    public var at: Int64
    public var undone: Bool
    public var changes: [Change]

    private enum CodingKeys: String, CodingKey { case id, at, undone, changes }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(Int64.self, forKey: .id)
        at = c.lenient(.at) ?? 0
        undone = c.lenient(.undone) ?? false
        changes = c.lenient(.changes) ?? []
    }
}

struct HqTidyUps: Decodable {
    var tidyUps: [HqTidyUp]

    private enum CodingKeys: String, CodingKey { case tidyUps }
    /// A host from before the "tidy" rename lists them as `dreams`.
    private enum OldKeys: String, CodingKey { case dreams }

    init(from decoder: Decoder) throws {
        let found: [HqTidyUp]? = try decoder.container(keyedBy: CodingKeys.self).lenient(.tidyUps)
        tidyUps = try found ?? decoder.container(keyedBy: OldKeys.self).lenient(.dreams) ?? []
    }
}

/// Thrown when the host has no `kithMemoryList` at all: it predates the memory files.
public struct HqMemoryUnsupported: LocalizedError {
    public init() {}
    public var errorDescription: String? { "Update Kith on your computer to see this agent's memory." }
}

/// A host that doesn't know a method answers with an error naming it.
public func isUnknownMethod(_ error: Error) -> Bool {
    error.localizedDescription.lowercased().contains("unknown method")
}

/// `kithMemoryUndoTidy`: files put back, and files left alone because they changed after the run.
public struct HqTidyUndo: Decodable, Hashable, Sendable {
    public struct Refusal: Decodable, Hashable, Sendable {
        public var file: String
        public var reason: String
    }
    public var restored: [String]
    public var refused: [Refusal]
    public var undone: Bool

    private enum CodingKeys: String, CodingKey { case restored, refused, undone }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        restored = c.lenient(.restored) ?? []
        refused = c.lenient(.refused) ?? []
        undone = c.lenient(.undone) ?? false
    }
}

extension HostClient {
    private struct FileBody: Encodable { var botId: String; var shared: Bool; var name: String }

    public func hqMemoryList(botId: String) async throws -> HqMemoryList {
        do {
            return try await call("kithMemoryList", ["botId": botId])
        } catch where isUnknownMethod(error) {
            throw HqMemoryUnsupported()
        }
    }

    public func hqMemoryRead(botId: String, shared: Bool, name: String) async throws -> HqMemoryText {
        try await call("kithMemoryRead", FileBody(botId: botId, shared: shared, name: name), timeout: 30)
    }

    public func hqMemorySave(botId: String, shared: Bool, name: String, text: String) async throws -> HqMemoryWrite {
        struct Body: Encodable { var botId: String; var shared: Bool; var name: String; var text: String }
        return try await call("kithMemorySave", Body(botId: botId, shared: shared, name: name, text: text), timeout: 30)
    }

    public func hqMemoryHistory(botId: String, shared: Bool, name: String) async throws -> HqMemoryHistory {
        try await call("kithMemoryHistory", FileBody(botId: botId, shared: shared, name: name), timeout: 30)
    }

    /// What `to` changed from `from`; both nil: the newest version against the one before it.
    public func hqMemoryDiff(botId: String, shared: Bool, name: String, from: Int64?, to: Int64?) async throws -> HqMemoryDiff {
        struct Body: Encodable { var botId: String; var shared: Bool; var name: String; var from: Int64?; var to: Int64? }
        return try await call("kithMemoryDiff", Body(botId: botId, shared: shared, name: name, from: from, to: to), timeout: 30)
    }

    public func hqMemoryRestore(botId: String, shared: Bool, name: String, version: Int64) async throws -> HqMemoryWrite {
        struct Body: Encodable { var botId: String; var shared: Bool; var name: String; var version: Int64 }
        return try await call("kithMemoryRestore", Body(botId: botId, shared: shared, name: name, version: version), timeout: 30)
    }

    /// Owner only. The deletion is a version, so `hqMemoryRestore` brings the file back; the answer is the version made.
    public func hqMemoryDelete(botId: String, shared: Bool, name: String) async throws -> HqMemoryWrite {
        try await call("kithMemoryDelete", FileBody(botId: botId, shared: shared, name: name), timeout: 30)
    }

    /// Owner only: keeps a fact out of every agent's memory (`kith memory forget`).
    public func hqMemoryForget(botId: String, text: String) async throws {
        struct Body: Encodable { var botId: String; var text: String }
        let _: Empty = try await call("kithMemoryForget", Body(botId: botId, text: text), timeout: 30)
    }

    public func hqTidyUps(botId: String, limit: Int = 20) async throws -> [HqTidyUp] {
        struct Body: Encodable { var botId: String; var limit: Int }
        do {
            let answer: HqTidyUps = try await call("kithMemoryTidyUps", Body(botId: botId, limit: limit), timeout: 30)
            return answer.tidyUps
        } catch where isUnknownMethod(error) {
            // Once, for a host that predates the rename.
            let answer: HqTidyUps = try await call("kithMemoryDreams", Body(botId: botId, limit: limit), timeout: 30)
            return answer.tidyUps
        }
    }

    public func hqUndoTidy(botId: String, id: Int64) async throws -> HqTidyUndo {
        struct Body: Encodable { var botId: String; var id: Int64 }
        do {
            return try await call("kithMemoryUndoTidy", Body(botId: botId, id: id), timeout: 30)
        } catch where isUnknownMethod(error) {
            return try await call("kithMemoryUndoDream", Body(botId: botId, id: id), timeout: 30)
        }
    }
}

private extension KeyedDecodingContainer {
    /// A value that is missing, null or of another type is nil.
    func lenient<T: Decodable>(_ key: Key) -> T? { (try? decodeIfPresent(T.self, forKey: key)) ?? nil }
}
