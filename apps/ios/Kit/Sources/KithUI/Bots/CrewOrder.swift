import Foundation

/// How Crew orders rooms below the pinned ones (Settings > Crew order).
public enum CrewOrderMode: String, CaseIterable, Sendable {
    /// Needs you, then working, then recent. The stored manual order breaks ties.
    case automatic
    /// Your drag order for everything, synced to the host exactly as before.
    case manual

    public static let storageKey = "crewOrder"

    public var label: String { self == .automatic ? "Automatic" : "Manual" }

    public init(stored: String) { self = CrewOrderMode(rawValue: stored) ?? .automatic }
}

/// What the ordering needs to know about one room.
public struct CrewOrderItem: Equatable, Sendable {
    public var id: String
    /// Where the room's computer sits in the computer order kept on this device.
    public var computer: Int
    public var pinned: Bool
    /// The stored manual order (the host's `position`).
    public var position: Int
    public var lastAt: Int64
    public var needsYou: Bool
    public var working: Bool

    public init(id: String, computer: Int = 0, pinned: Bool, position: Int, lastAt: Int64, needsYou: Bool = false, working: Bool = false) {
        self.id = id
        self.computer = computer
        self.pinned = pinned
        self.position = position
        self.lastAt = lastAt
        self.needsYou = needsYou
        self.working = working
    }
}

public enum CrewOrder {
    /// Room ids in display order. Pinned rooms are always first, in the user's own order (position, then recent).
    /// Automatic puts the rest by need (needs you, working, recent) and uses the stored position only to break ties.
    public static func sorted(_ items: [CrewOrderItem], mode: CrewOrderMode) -> [String] {
        items.sorted { a, b in
            if a.pinned != b.pinned { return a.pinned }
            if a.pinned || mode == .manual { return manual(a, b) }
            let (ba, bb) = (bucket(a), bucket(b))
            if ba != bb { return ba < bb }
            if a.lastAt != b.lastAt { return a.lastAt > b.lastAt }
            return manual(a, b)
        }.map(\.id)
    }

    private static func bucket(_ item: CrewOrderItem) -> Int { item.needsYou ? 0 : item.working ? 1 : 2 }

    /// Computers in their stored order, then each computer's own order: positions are per computer, so interleaving
    /// two computers' rooms by position would put a dropped room where it can't be written.
    private static func manual(_ a: CrewOrderItem, _ b: CrewOrderItem) -> Bool {
        if a.computer != b.computer { return a.computer < b.computer }
        if a.position != b.position { return a.position < b.position }
        if a.lastAt != b.lastAt { return a.lastAt > b.lastAt }
        return a.id < b.id
    }

    /// Whether dragging (and Move up / Move down) is offered for a room: always on pinned rooms, on every room in Manual.
    public static func canReorder(pinned: Bool, mode: CrewOrderMode) -> Bool { pinned || mode == .manual }

    /// Whether an unpinned room can be dragged up into the pinned area to pin it: Automatic only (Manual drags
    /// already move everything, and a drop there never changes what is pinned).
    public static func canDragToPin(pinned: Bool, mode: CrewOrderMode) -> Bool { !pinned && mode == .automatic }

    /// What a drop means in Automatic, where pinned and unpinned rooms are one list (pinned first). `source` and
    /// `destination` are the `onMove` indexes into that list. A pinned room stays among the pinned; an unpinned one
    /// dropped strictly inside the pinned area is pinned there; anywhere else nothing changes, so a drop on the
    /// boundary never pins by accident and the automatic order never takes a manual position.
    public static func drop(pinnedCount: Int, source: Int, destination: Int) -> Drop {
        if source < pinnedCount { return .reorderPinned(from: source, to: min(destination, pinnedCount)) }
        return destination < pinnedCount ? .pin(unpinnedIndex: source - pinnedCount, at: max(destination, 0)) : .ignore
    }

    public enum Drop: Equatable, Sendable {
        /// Move within the pinned rooms (`Array.move(fromOffsets:toOffset:)` semantics).
        case reorderPinned(from: Int, to: Int)
        /// Pin the unpinned room at this index of the unpinned rooms, as the pinned room at index `at`.
        case pin(unpinnedIndex: Int, at: Int)
        case ignore
    }
}

/// An agent put to sleep from its menu: shown asleep until it next works or gets a message. The host's `stop` ends
/// what it is doing, which can itself bring the agent idle and a notice with it, so the first idle reading only
/// settles the baseline; anything after that wakes it.
public struct CrewSleep: Equatable, Sendable {
    public private(set) var lastAt: Int64
    public private(set) var settled = false
    public private(set) var awake = false

    public init(lastAt: Int64) { self.lastAt = lastAt }

    /// Feed the agent's current state each time it changes.
    public mutating func observe(working: Bool, lastAt now: Int64) {
        guard !awake else { return }
        if !settled {
            if !working { settled = true; lastAt = now }
            return
        }
        if working || now > lastAt { awake = true }
    }
}

/// Holds the list still while a finger is on it: while `frozen`, rooms keep the order they were shown in (rooms that
/// are gone drop out, new ones go last); once it lets go, the computed order is adopted.
public struct CrewOrderHold: Equatable, Sendable {
    public private(set) var shown: [String] = []

    public init() {}

    /// What to show now, without remembering it (a view body reads this).
    public func resolved(computed: [String], frozen: Bool) -> [String] {
        guard frozen, !shown.isEmpty else { return computed }
        let alive = Set(computed)
        let kept = shown.filter(alive.contains)
        let keptSet = Set(kept)
        return kept + computed.filter { !keptSet.contains($0) }
    }

    /// Remembers what is shown; call when the computed order or `frozen` changes.
    @discardableResult
    public mutating func update(computed: [String], frozen: Bool) -> [String] {
        shown = resolved(computed: computed, frozen: frozen)
        return shown
    }
}

/// One message of a room the dock's box can find, and land on in the chat.
public struct CrewSearchMessage: Equatable, Sendable {
    /// The chat entry's id.
    public var id: String
    public var text: String

    public init(id: String, text: String) {
        self.id = id
        self.text = text
    }
}

/// What the dock's box searches: one room's names and status.
public struct CrewSearchEntry: Equatable, Sendable {
    public enum Kind: Equatable, Sendable { case agent, team, group }

    public var id: String
    public var kind: Kind
    public var name: String
    /// The person a team room is shared with, or a group's member names.
    public var people: [String]
    public var status: String
    /// Folder, harness and the like: matched last.
    public var extras: [String]
    /// The room's messages, newest first.
    public var messages: [CrewSearchMessage]

    public init(id: String, kind: Kind = .agent, name: String, people: [String] = [], status: String = "", extras: [String] = [],
                messages: [CrewSearchMessage] = []) {
        self.id = id
        self.kind = kind
        self.messages = messages
        self.name = name
        self.people = people
        self.status = status
        self.extras = extras
    }
}

/// Where a result sits once the list is grouped (the keyboard dropped).
public enum CrewSearchGroup: CaseIterable, Equatable, Sendable {
    case people, groups, agents, topics

    public var title: String {
        switch self {
        case .people: "People"
        case .groups: "Groups"
        case .agents: "Agents"
        case .topics: "Topics"
        }
    }
}

/// One matching room: where it groups, and the message the match was in (the chat lands there) when the room was
/// found by what was said in it and not by its name, person or status.
public struct CrewSearchHit: Equatable, Sendable {
    public var id: String
    public var group: CrewSearchGroup
    public var message: CrewSearchMessage?
}

public enum CrewSearch {
    /// Ids of the rooms matching `query`, best first. Every word of the query must match somewhere. A name beats a
    /// person, which beats the status line, which beats the rest, which beats a message; at equal rank the list
    /// keeps the order it came in.
    public static func rank(_ entries: [CrewSearchEntry], query: String) -> [String] {
        hits(entries, query: query).map(\.id)
    }

    /// `rank` with each room's group and matching message.
    public static func hits(_ entries: [CrewSearchEntry], query: String) -> [CrewSearchHit] {
        let words = query.lowercased().split(whereSeparator: \.isWhitespace).map(String.init)
        guard !words.isEmpty else { return entries.map { CrewSearchHit(id: $0.id, group: group(of: $0), message: nil) } }
        let scored = entries.enumerated().compactMap { index, entry -> (Int, Int, CrewSearchHit)? in
            var total = 0
            var direct = true
            for word in words {
                let score = score(word, in: entry)
                if score > 0 {
                    total += score
                } else if entry.messages.contains(where: { $0.text.lowercased().contains(word) }) {
                    // Not in the room's names: what was said in it has the word.
                    direct = false
                    total += 1
                } else {
                    return nil
                }
            }
            let hit = direct ? CrewSearchHit(id: entry.id, group: group(of: entry), message: nil)
                : CrewSearchHit(id: entry.id, group: .topics, message: message(in: entry, words: words))
            return (total, index, hit)
        }
        return scored.sorted { $0.0 != $1.0 ? $0.0 > $1.0 : $0.1 < $1.1 }.map(\.2)
    }

    /// The hits under People, Groups, Agents and Topics, in that order; a group with nothing in it is left out.
    public static func grouped(_ hits: [CrewSearchHit]) -> [(group: CrewSearchGroup, hits: [CrewSearchHit])] {
        CrewSearchGroup.allCases.compactMap { group in
            let inGroup = hits.filter { $0.group == group }
            return inGroup.isEmpty ? nil : (group, inGroup)
        }
    }

    private static func group(of entry: CrewSearchEntry) -> CrewSearchGroup {
        switch entry.kind {
        case .agent: .agents
        case .team: .people
        case .group: .groups
        }
    }

    /// The newest message holding every word; nil when the words are spread over several messages.
    private static func message(in entry: CrewSearchEntry, words: [String]) -> CrewSearchMessage? {
        entry.messages.first { message in
            let text = message.text.lowercased()
            return words.allSatisfy { text.contains($0) }
        }
    }

    private static func score(_ word: String, in entry: CrewSearchEntry) -> Int {
        var best = textScore(word, entry.name) * 10
        for person in entry.people { best = max(best, textScore(word, person) * 6) }
        best = max(best, textScore(word, entry.status) * 3)
        for extra in entry.extras { best = max(best, textScore(word, extra)) }
        return best
    }

    /// 3 a name that starts with it, 2 a word that does, 1 contains it, 0 no match.
    private static func textScore(_ word: String, _ text: String) -> Int {
        let text = text.lowercased()
        if text.hasPrefix(word) { return 3 }
        if text.split(whereSeparator: { !$0.isLetter && !$0.isNumber }).contains(where: { $0.hasPrefix(word) }) { return 2 }
        return text.contains(word) ? 1 : 0
    }
}
