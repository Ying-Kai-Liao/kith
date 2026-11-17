import KithKit
import Foundation

/// An invited person's rooms (SPEC §8-§10). Each room in `myRooms` is a group in the roster; the host sends nothing to
/// people on its events stream, so a room is read on open and on foreground, and its list on connect.
extension BotStore {
    func refreshMyRooms() async {
        guard !isOwner, let client, !retired else { return }
        let rooms: [HostContract.MyRoom]
        do {
            rooms = try await client.myRooms()
        } catch {
            return // a host before rooms with people (or with the setting off) has none
        }
        // What a launch restored too: a room the list does not name is closed, and so is a cached group bot with no room behind it.
        let cachedRooms = MemberRoomLogic.cachedRoomIds(restored: Array(myRooms.keys), bots: Array(bots.values))
        for id in MemberRoomLogic.closed(before: cachedRooms, after: rooms) { closeRoom(id) }
        for room in rooms {
            closedRooms[room.id] = nil
            // A removed-and-added-again person has a new stay: what the old one showed is dropped before the room is read again.
            if MemberRoomLogic.stayChanged(known: myRooms[room.id], fresh: room) {
                entries[room.id] = nil
                historyComplete.remove(room.id)
            }
            let last = chat(room.id).last { $0.seq != Int64.max }
            if let bot = MemberRoomLogic.bot(for: room, lastAt: last?.createdAt, lastMessage: last?.data.text) { bots[room.id] = bot }
        }
        myRooms = Dictionary(uniqueKeysWithValues: rooms.map { ($0.id, $0) })
        scheduleSave()
    }

    /// The room is gone for this person (left, removed, closed): the cache is dropped and the view says so.
    func closeRoom(_ id: String) {
        if let name = bots[id]?.name ?? myRooms[id]?.name { closedRooms[id] = name }
        myRooms[id] = nil
        bots[id] = nil
        entries[id] = nil
        if selection == id { selection = nil }
        scheduleSave()
    }

    /// The messages the person may see; a refusal means the room closed under them.
    func loadRoom(_ id: String) async {
        guard let client, myRooms[id] != nil else { return }
        do {
            for e in try await client.history(botId: id, beforeSeq: Int64.max, limit: 100) { upsert(e) }
        } catch {
            if MemberRoomLogic.isClosedRefusal(error) { closeRoom(id) } else { await refreshMyRooms() }
        }
    }

    func leaveRoom(_ id: String) async {
        do {
            try await client?.leaveRoom(id)
            closeRoom(id)
        } catch {
            lastError = error.localizedDescription
            await refreshMyRooms()
        }
    }
}
