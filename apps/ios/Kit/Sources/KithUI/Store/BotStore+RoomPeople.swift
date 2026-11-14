import KithKit
import Foundation

/// What a group room's chat needs about its people: the stays (for "From before Ben joined") and who they are.
extension BotStore {
    /// The owner's copy of a room's people; a member, an older host and a host with the setting off leave it empty.
    func refreshRoomPeople(_ room: String) async {
        guard isOwner, let client else { return }
        do {
            let r = try await client.roomPeople(room: room)
            roomPeopleEnabled[room] = r.enabled
            roomStays[room] = r.people
            if r.enabled { roomPeopleList = try await client.people() }
        } catch {
            roomPeopleEnabled[room] = false
        }
    }

    /// The owner's own message put back in the box, not sent ("Send again").
    public func sendAgain(_ text: String, in botId: String) {
        setComposerDraft(text, for: botId)
    }

    /// The room's agents and what the "won't answer here" line says, nil while nothing is wrong or the setting is off.
    public func wontAnswerLine(in group: Bot) -> String? {
        guard isOwner, roomPeopleEnabled[group.id] == true else { return nil }
        let parts = RoomPeopleLogic.parts(members: group.members, find: { bots[$0] }, outsideName: { _ in "Bot" })
        let rows = RoomPeopleLogic.needsKai(stays: roomStays[group.id] ?? [], people: roomPeopleList, agents: parts.agents)
        return RoomPeopleLogic.wontAnswerLine(rows, agents: parts.agents)
    }

    func personName(_ id: String) -> String { roomPeopleList.first { $0.personId == id }?.name ?? "Someone" }
}
