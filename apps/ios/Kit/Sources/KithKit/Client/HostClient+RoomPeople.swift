import Foundation

/// People in group rooms (serve/host/src/api/room_people.rs). The owner adds and removes; an invited person lists
/// their rooms and leaves. A host before rooms with people answers "unknown method" (`InviteProof.isUnknownMethod`).
public protocol RoomPeopleHost: Sendable {
    func roomPeople(room: String) async throws -> HostContract.RoomPeople
    func addRoomPerson(room: String, person: String) async throws
    func removeRoomPerson(room: String, person: String) async throws
}

public extension HostClient {
    func roomPeople(room: String) async throws -> HostContract.RoomPeople {
        try await call(.roomPeople, HostContract.RoomPeopleParams(room: room))
    }

    /// The host's refusal text is user-ready (an outside bot in the room, a missing grant); it is thrown as is.
    func addRoomPerson(room: String, person: String) async throws {
        let _: HostContract.RoomPersonChange = try await call(.addRoomPerson, HostContract.AddRoomPersonParams(room: room, person: person), timeout: 60)
    }

    func removeRoomPerson(room: String, person: String) async throws {
        let _: HostContract.RoomPersonChange = try await call(.removeRoomPerson, HostContract.RemoveRoomPersonParams(room: room, person: person), timeout: 60)
    }

    /// An invited person's rooms right now, each with the stay its posts must carry.
    func myRooms() async throws -> [HostContract.MyRoom] {
        let res: HostContract.MyRooms = try await call(.myRooms)
        return res.rooms
    }

    func leaveRoom(_ room: String) async throws {
        let _: HostContract.RoomLeft = try await call(.leaveRoom, HostContract.LeaveRoomParams(room: room))
    }

    /// `createInvite` that also adds the person to these group rooms once they accept.
    func createInvite(name: String?, personId: String?, agents: [String: InviteRole], rooms: [String]) async throws -> CreatedInvite {
        struct Body: Encodable { var name: String?; var personId: String?; var agents: [String: InviteRole]; var rooms: [String]? }
        return try await call("createInvite", Body(name: name, personId: personId, agents: agents, rooms: rooms.isEmpty ? nil : rooms), timeout: 60)
    }
}

extension HostClient: RoomPeopleHost {}
