import KithKit
import Foundation

/// What a Crew row is, for the chips.
public enum CrewRowKind: Equatable, Sendable {
    case agent
    /// A group room.
    case group
    /// A room on a computer joined by invite: shared with the person who owns it.
    case team
    /// A person from the people store.
    case person
}

/// The chips above Crew: All, People, Agents, Groups.
public enum CrewFilter: String, CaseIterable, Sendable {
    case all = "All"
    case people = "People"
    case agents = "Agents"
    case groups = "Groups"

    public var title: String { rawValue }

    public func shows(_ kind: CrewRowKind) -> Bool {
        switch self {
        case .all: true
        case .people: kind == .person || kind == .team
        case .agents: kind == .agent
        case .groups: kind == .group
        }
    }

    /// The chips to offer. A guest account (joined by invite only) has no people store, so no People chip.
    public static func available(hasPeople: Bool) -> [CrewFilter] {
        hasPeople ? allCases : allCases.filter { $0 != .people }
    }

    /// Ids kept by the filter, in their given order. Pins don't exempt a row: a pinned agent is hidden under People.
    public static func apply(_ filter: CrewFilter, to ids: [String], kind: (String) -> CrewRowKind?) -> [String] {
        ids.filter { id in kind(id).map(filter.shows) ?? false }
    }
}

/// One person row of Crew, from the people store of an owned computer. Not a room: tapping it opens the person.
public struct CrewPerson: Identifiable, Equatable, Sendable {
    public var computerId: ComputerID
    public var person: HostPerson

    public init(computerId: ComputerID, person: HostPerson) {
        self.computerId = computerId
        self.person = person
    }

    public static func rowId(computerId: ComputerID, personId: String) -> String { "person/\(computerId)/\(personId)" }

    public var id: String { Self.rowId(computerId: computerId, personId: person.personId) }

    /// Their latest sign of life: the newest device seen. Zero when they never connected (sorts last).
    public var lastAt: Int64 { person.devices.compactMap(\.lastSeenAt).max() ?? 0 }

    public var suspended: Bool { person.revoked }

    public var orderItem: CrewOrderItem {
        CrewOrderItem(id: id, pinned: false, position: 0, lastAt: lastAt, isPerson: true)
    }

    public var rowModel: CrewRowModel {
        // Who they use, not a count; no time, since last seen is not a message and must not read like one.
        let names = person.agents.keys.sorted()
        let status = suspended ? "Access paused" : names.isEmpty ? "No agents yet" : "Uses " + names.joined(separator: ", ")
        let trailing: String? = suspended ? "Paused" : nil
        return CrewRowModel(id: id, shape: .person(personId: person.personId), name: person.name, trailing: trailing, status: status,
                            dimmed: suspended)
    }

    /// Everyone from several computers' people lists, each record once (two records of one name stay two rows).
    /// Someone who hasn't joined yet is a pending invite, not a person.
    public static func rows(_ lists: [(ComputerID, [HostPerson])]) -> [CrewPerson] {
        lists.flatMap { computerId, people in people.filter(\.joined).map { CrewPerson(computerId: computerId, person: $0) } }
    }
}
