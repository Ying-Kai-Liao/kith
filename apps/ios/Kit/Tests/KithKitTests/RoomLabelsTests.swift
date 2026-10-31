import Foundation
import Testing
@testable import KithKit

@Suite struct RoomLabelsTests {
    private func bot(_ kind: String, _ labels: [String] = []) throws -> Bot {
        let json = try JSONSerialization.data(withJSONObject: ["id": UUID().uuidString, "kind": kind, "labels": labels])
        return try JSONDecoder().decode(Bot.self, from: json)
    }

    @Test func typedLabelsAreLowercasedTrimmedDeduplicatedAndCapped() {
        #expect(RoomLabels.parse(" Launch, q4  Plan ,,launch\nOps") == ["launch", "q4 plan", "ops"])
        #expect(RoomLabels.parse("a,b,c,d,e,f,g").count == RoomLabels.maxCount)
        #expect(RoomLabels.parse(String(repeating: "x", count: 40))[0].count == RoomLabels.maxLength)
        #expect(RoomLabels.parse("").isEmpty)
    }

    @Test func theFilterRowListsLabelsRoomsUseMostUsedFirst() throws {
        let bots = [try bot("group", ["ops", "launch"]), try bot("group", ["launch"]), try bot("group"), try bot("agent", ["stray"])]
        #expect(RoomLabels.inUse(bots) == ["launch", "ops"])
    }

    @Test func aLabelFilterKeepsTheRoomsThatCarryItAndNoAgent() throws {
        #expect(RoomLabels.matches(try bot("group", ["ops"]), label: "ops"))
        #expect(!RoomLabels.matches(try bot("group", ["ops"]), label: "launch"))
        #expect(!RoomLabels.matches(try bot("group"), label: "ops"))
        #expect(!RoomLabels.matches(try bot("agent"), label: "ops"))
        #expect(RoomLabels.matches(try bot("agent"), label: nil))
    }

    @Test func aBotFromAnOlderHostHasNoLabels() throws {
        #expect(try bot("group").labels.isEmpty)
        let data = Data(#"{"id":"g","kind":"group","labels":["a"]}"#.utf8)
        #expect(try JSONDecoder().decode(Bot.self, from: data).labels == ["a"])
    }
}
