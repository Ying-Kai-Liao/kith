import Foundation

/// A topic the owner split off: set by the host on its first message (the root). Every field is optional-tolerant
/// so a newer host's topic still decodes.
public struct EntryTopic: Codable, Hashable, Sendable {
    public var id: String
    public var title: String
    /// The last message the topic held when it was made, and its time.
    public var untilId: String?
    public var untilAt: Int64
    /// Turns whose later entries fold in too (a reply still being written when the topic was made).
    public var turns: [Int64]
    public var createdAt: Int64

    public init(id: String, title: String, untilId: String? = nil, untilAt: Int64, turns: [Int64] = [], createdAt: Int64 = 0) {
        self.id = id
        self.title = title
        self.untilId = untilId
        self.untilAt = untilAt
        self.turns = turns
        self.createdAt = createdAt
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        title = try c.decodeIfPresent(String.self, forKey: .title) ?? ""
        untilId = try c.decodeIfPresent(String.self, forKey: .untilId)
        untilAt = try c.decodeIfPresent(Int64.self, forKey: .untilAt) ?? 0
        turns = try c.decodeIfPresent([Int64].self, forKey: .turns) ?? []
        createdAt = try c.decodeIfPresent(Int64.self, forKey: .createdAt) ?? 0
    }
}

/// A topic and the entries it folds, the root first.
public struct TopicGroup: Hashable, Sendable {
    public let root: Entry
    public let topic: EntryTopic
    /// Every entry of the topic by time (ties keep their arrival order), the root included.
    public let members: [Entry]

    public var id: String { "topic-\(root.id)" }
    public var date: Date { root.date }

    public func contains(_ entryId: String) -> Bool {
        members.contains { $0.id == entryId }
    }
}

/// What a topic's card shows.
public struct TopicSummary: Hashable, Sendable {
    /// Up to two distinct authors, as `user` or `agent:<bot id>` (an agent with no author is `agent:`).
    public var authors: [String]
    /// The newest user or agent message, one line, and who wrote it.
    public var latest: Entry?
    public var latestLine: String
    /// User and agent messages: no tool rows or notices.
    public var count: Int
    public var date: Date
    /// Permission cards waiting on the owner.
    public var needsYou: [Entry]
    /// A folded reply or tool call is still being written or run.
    public var working: Bool
}

public enum Topics {
    /// Whether `entry` folds into the topic rooted at `root` (host rule): inside `root.createdAt ... untilAt`, or in a
    /// listed turn after the root. Same millisecond counts as in range.
    public static func folds(_ entry: Entry, into root: Entry, topic: EntryTopic) -> Bool {
        if entry.id == root.id { return true }
        if entry.createdAt >= root.createdAt && entry.createdAt <= topic.untilAt { return true }
        return topic.turns.contains(entry.turn) && entry.createdAt > root.createdAt
    }

    /// The topics in a chat's flat view. A topic whose root isn't loaded has nothing to fold. An entry belongs to the
    /// first (oldest) topic that takes it, and a root already inside an earlier topic is not a topic of its own.
    public static func groups(in entries: [Entry]) -> [TopicGroup] {
        var seen = Set<String>()
        let unique = entries.filter { seen.insert($0.id).inserted }
        let roots = unique.filter { $0.data.topic != nil }.sorted { ($0.createdAt, $0.seq) < ($1.createdAt, $1.seq) }
        guard !roots.isEmpty else { return [] }
        var taken = Set<String>()
        var out: [TopicGroup] = []
        for root in roots {
            guard let topic = root.data.topic, !taken.contains(root.id) else { continue }
            let members = unique.enumerated()
                .filter { _, e in !taken.contains(e.id) && folds(e, into: root, topic: topic) }
                .sorted { a, b in a.element.createdAt == b.element.createdAt ? a.offset < b.offset : a.element.createdAt < b.element.createdAt }
                .map(\.element)
            taken.formUnion(members.map(\.id))
            out.append(TopicGroup(root: root, topic: topic, members: members))
        }
        return out
    }

    public static func summary(of group: TopicGroup) -> TopicSummary {
        // Same set as the desktop card: every user and agent entry, streaming or not.
        let said = group.members.filter { $0.kind == "user" || $0.kind == "agent" }
        var authors: [String] = []
        for e in said {
            let key = e.kind == "user" ? "user" : "agent:\(e.data.author ?? "")"
            if !authors.contains(key) { authors.append(key) }
            if authors.count == 2 { break }
        }
        // Newest entry with text; tool-only agent rows have none and never become the latest line.
        var latest: Entry?
        var line = ""
        for e in said.reversed() {
            let l = plainLine(e.data.text ?? "")
            if !l.isEmpty { latest = e; line = l; break }
        }
        return TopicSummary(
            authors: authors, latest: latest, latestLine: line, count: said.count,
            date: (group.members.last ?? group.root).date,
            needsYou: group.members.filter { $0.kind == "permission" && $0.data.status == "pending" },
            working: group.members.contains {
                ($0.kind == "agent" && $0.data.status == "streaming") || ($0.kind == "tool" && $0.data.status == "in_progress")
            })
    }

    /// The first line of `text` with inline markdown removed: code spans, bold/italic, links, and leading #/>/- markers.
    public static func plainLine(_ text: String) -> String {
        guard let first = text.split(whereSeparator: \.isNewline).first(where: { !$0.trimmingCharacters(in: .whitespaces).isEmpty }) else { return "" }
        var s = String(first).trimmingCharacters(in: .whitespaces)
        func sub(_ pattern: String, _ template: String) {
            s = s.replacingOccurrences(of: pattern, with: template, options: .regularExpression)
        }
        sub(#"^(?:#{1,6}\s+|>\s*|[-*+]\s+)+"#, "")
        sub(#"!?\[([^\]]*)\]\([^)]*\)"#, "$1")
        sub(#"`+([^`]*)`+"#, "$1")
        sub(#"(\*\*|__)(.+?)\1"#, "$2")
        sub(#"\*([^*\s][^*]*?)\*"#, "$1")
        sub(#"(?<![A-Za-z0-9])_([^_\s][^_]*?)_(?![A-Za-z0-9])"#, "$1")
        return s.trimmingCharacters(in: .whitespaces)
    }

    /// The prefilled title for a topic made from `entry`: its first line, trimmed to the host's 80 characters.
    public static func suggestedTitle(for entry: Entry) -> String {
        let first = (entry.data.text ?? "").split(whereSeparator: \.isNewline).first.map(String.init) ?? ""
        return String(first.trimmingCharacters(in: .whitespaces).prefix(titleLimit))
    }

    public static let titleLimit = 80
}

extension HostClient {
    /// Splits a topic off at this message (owner only). The answer is the root entry, which also arrives as an update.
    public func makeTopic(botId: String, entryId: String, title: String) async throws {
        struct Reply: Decodable { var entry: Entry? }
        let _: Reply = try await call(.makeTopic, HostContract.MakeTopicParams(botId: botId, entryId: entryId, title: title))
    }

    public func renameTopic(botId: String, entryId: String, title: String) async throws {
        struct Reply: Decodable { var entry: Entry? }
        let _: Reply = try await call(.renameTopic, HostContract.RenameTopicParams(botId: botId, entryId: entryId, title: title))
    }

    public func unfoldTopic(botId: String, entryId: String) async throws {
        struct Reply: Decodable { var entry: Entry? }
        let _: Reply = try await call(.unfoldTopic, HostContract.UnfoldTopicParams(botId: botId, entryId: entryId))
    }
}
