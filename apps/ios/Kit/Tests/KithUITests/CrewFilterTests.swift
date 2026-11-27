import Foundation
import KithKit
import Testing
@testable import KithUI

private func room(_ id: String, pinned: Bool = false, position: Int = 0, lastAt: Int64 = 0, needsYou: Bool = false) -> CrewOrderItem {
    CrewOrderItem(id: id, pinned: pinned, position: position, lastAt: lastAt, needsYou: needsYou)
}

private func person(_ id: String, lastAt: Int64 = 0) -> CrewOrderItem {
    CrewOrderItem(id: id, pinned: false, position: 0, lastAt: lastAt, isPerson: true)
}

@Test func peopleSortByRecencyAmongTheRestInAutomatic() {
    let items = [room("old", lastAt: 1), person("sam", lastAt: 5), room("new", lastAt: 9), person("jo", lastAt: 0)]
    // A person with no activity sorts last.
    #expect(CrewOrder.sorted(items, mode: .automatic) == ["new", "sam", "old", "jo"])
    #expect(CrewOrder.sorted(items + [room("asks", needsYou: true)], mode: .automatic).first == "asks")
}

@Test func peopleFollowEveryRoomInManualAndKeepRecencyAmongThemselves() {
    let items = [person("jo", lastAt: 2), room("b", position: 1), person("sam", lastAt: 7), room("a", position: 0)]
    #expect(CrewOrder.sorted(items, mode: .manual) == ["a", "b", "sam", "jo"])
}

@Test func pinnedRoomsStayAbovePeople() {
    let items = [person("sam", lastAt: 99), room("p", pinned: true)]
    #expect(CrewOrder.sorted(items, mode: .automatic) == ["p", "sam"])
}

@Test func chipsFilterByKind() {
    #expect(CrewFilter.all.shows(.person) && CrewFilter.all.shows(.group))
    #expect(CrewFilter.people.shows(.person) && CrewFilter.people.shows(.team) && !CrewFilter.people.shows(.agent))
    #expect(CrewFilter.agents.shows(.agent) && !CrewFilter.agents.shows(.group) && !CrewFilter.agents.shows(.person))
    #expect(CrewFilter.groups.shows(.group) && !CrewFilter.groups.shows(.agent))
}

@Test func filterAppliesToPinnedRoomsToo() {
    let kinds: [String: CrewRowKind] = ["p": .agent, "g": .group, "sam": .person]
    #expect(CrewFilter.apply(.groups, to: ["p", "g", "sam"], kind: { kinds[$0] }) == ["g"])
    #expect(CrewFilter.apply(.people, to: ["p", "g", "sam"], kind: { kinds[$0] }) == ["sam"])
    #expect(CrewFilter.apply(.all, to: ["p", "g", "sam"], kind: { kinds[$0] }) == ["p", "g", "sam"])
}

@Test func guestAccountHasNoPeopleChip() {
    #expect(CrewFilter.available(hasPeople: false) == [.all, .agents, .groups])
    #expect(CrewFilter.available(hasPeople: true) == [.all, .people, .agents, .groups])
}

@Test func personRowsShowPausedAndNeverMergeByName() throws {
    let json = """
    [{"personId":"p1","name":"Sam","agents":{"helper":"member"},"joined":true,"devices":[{"key":"d","lastSeenAt":5000}]},
     {"personId":"p2","name":"Sam","joined":true,"revoked":true}]
    """
    let people = try JSONDecoder().decode([HostPerson].self, from: Data(json.utf8))
    let rows = CrewPerson.rows([("mac-a", [people[0]]), ("mac-b", [people[1]])])
    #expect(rows.count == 2)
    #expect(rows[0].lastAt == 5_000 && rows[1].lastAt == 0)
    #expect(rows[1].rowModel.trailing == "Paused" && rows[1].rowModel.dimmed)
    #expect(!rows[0].rowModel.dimmed)
    #expect(rows[0].id != rows[1].id)
}
