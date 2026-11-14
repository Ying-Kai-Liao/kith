import KithKit
import Foundation
import Observation

/// One group room's people as the owner sees them: every stay, whether this host turned people in rooms on, and the
/// changes (add, remove, give access and add). Reads `people` and the invites through its `PeopleStore`.
@MainActor
@Observable
public final class RoomPeopleStore {
    public let room: String
    public let people: PeopleStore
    /// nil until the host answered; false while the setting is off or the host is from before rooms with people.
    public private(set) var enabled: Bool?
    public private(set) var stays: [RoomStay] = []
    public private(set) var busy = false
    public var error: String?

    private let host: @MainActor () -> (any RoomPeopleHost)?

    public init(room: String, people: PeopleStore, host: @escaping @MainActor () -> (any RoomPeopleHost)?) {
        self.room = room
        self.people = people
        self.host = host
    }

    public convenience init(room: String, store: BotStore, people: PeopleStore? = nil) {
        self.init(room: room, people: people ?? PeopleStore(store: store), host: { [weak store] in store?.client })
    }

    /// Reads both lists (the stays and who has which agent): when Room info opens and after every change.
    public func load() async {
        await people.load()
        guard let host = host() else { return }
        do {
            let r = try await host.roomPeople(room: room)
            enabled = r.enabled
            stays = r.people
        } catch {
            if InviteProof.isUnknownMethod(error) { enabled = false } else { self.error = error.localizedDescription }
        }
    }

    /// Runs one change at a time (a second tap while one is on its way does nothing), then re-reads. True when it went through.
    @discardableResult
    private func run(_ work: (any RoomPeopleHost) async throws -> Void) async -> Bool {
        guard !busy, let host = host() else { return false }
        busy = true
        error = nil
        var ok = false
        do {
            try await work(host)
            ok = true
        } catch {
            self.error = error.localizedDescription
        }
        await load()
        busy = false
        return ok
    }

    public func add(_ personId: String) async -> Bool {
        await run { try await $0.addRoomPerson(room: room, person: personId) }
    }

    public func remove(_ personId: String) async -> Bool {
        await run { try await $0.removeRoomPerson(room: room, person: personId) }
    }

    /// Gives agents without adding (the needs-you row's "Give coder").
    public func give(_ person: HostPerson, _ given: [String: InviteRole]) async -> Bool {
        await run { _ in try await self.save(person, given) }
    }

    /// The grant card's one step: save access (merged with what the person holds), then add. A failed add says both
    /// outcomes; the grant is never undone silently.
    public func grantAndAdd(_ person: HostPerson, _ given: [String: InviteRole], names: [String]) async -> Bool {
        await run { host in
            try await self.save(person, given)
            do {
                try await host.addRoomPerson(room: self.room, person: person.personId)
            } catch {
                throw RoomPeopleError("\(person.name) now has \(names.joined(separator: " and ")), but couldn't join: \(error.localizedDescription)")
            }
        }
    }

    private func save(_ person: HostPerson, _ given: [String: InviteRole]) async throws {
        guard await people.writeAccess(personId: person.personId, agents: RoomPeopleLogic.grantRoles(person, given: given)) else {
            throw RoomPeopleError(people.error ?? "Couldn't give access.")
        }
    }
}

struct RoomPeopleError: LocalizedError {
    var message: String
    init(_ message: String) { self.message = message }
    var errorDescription: String? { message }
}
