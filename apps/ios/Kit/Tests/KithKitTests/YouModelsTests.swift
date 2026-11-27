import Foundation
import Testing
@testable import KithKit

private func routine(_ id: String, _ bot: String, _ name: String) throws -> Routine {
    let json = #"{"id":"\#(id)","botId":"\#(bot)","name":"\#(name)","instruction":"","triggers":[],"enabled":true,"createdAt":0,"updatedAt":0,"triggerDescriptions":[]}"#
    return try JSONDecoder().decode(Routine.self, from: Data(json.utf8))
}

private func people(_ agent: String, _ spend: [String: Double]) throws -> AgentPeople {
    let body = spend.map { "\"\($0.key)\":\($0.value)" }.joined(separator: ",")
    let json = #"{"agent":"\#(agent)","month":"November","people":{\#(body)},"budgets":{}}"#
    return try JSONDecoder().decode(AgentPeople.self, from: Data(json.utf8))
}

@Test func routinesGroupByAgentSortedWithOtherLast() throws {
    let all = [try routine("1", "b2", "Weekly"), try routine("2", "b1", "Daily"), try routine("3", "gone", "Old"),
               try routine("4", "b1", "Backlog"), try routine("5", "room", "Room one")]
    let groups = RoutineGroup.group(all, names: ["b1": "writer", "b2": "helper"])
    #expect(groups.map(\.title) == ["helper", "writer", "Other"])
    #expect(groups[1].routines.map(\.name) == ["Backlog", "Daily"])
    #expect(groups[2].isOther)
    #expect(groups[2].routines.map(\.name) == ["Old", "Room one"])
}

@Test func routinesGroupEmptyAndNoOtherWhenAllKnown() throws {
    #expect(RoutineGroup.group([], names: ["b": "helper"]).isEmpty)
    let groups = RoutineGroup.group([try routine("1", "b", "Daily")], names: ["b": "helper"])
    #expect(groups.count == 1 && !groups[0].isOther)
}

@Test func peopleSplitSumsAcrossAgentsAndNamesTheOwner() throws {
    let totals = PersonTotal.combine([
        try people("helper", ["owner": 10, "Sam": 1.5, "Jo": 0]),
        try people("writer", ["owner": 2.25, "Sam": 4]),
    ])
    #expect(totals.map(\.name) == ["You", "Sam"])
    #expect(totals[0].total == 12.25)
    #expect(totals[1].total == 5.5)
    #expect(totals[1].agents.map(\.agent) == ["writer", "helper"])
}

@Test func peopleSplitWithNoSpendIsEmpty() throws {
    #expect(PersonTotal.combine([]).isEmpty)
    #expect(PersonTotal.combine([try people("helper", ["owner": 0])]).isEmpty)
}

@Test func planMetersTodayAreOneAccountPerProvider() throws {
    let json = #"{"providers":[{"id":"claude","name":"Claude","windows":[],"source":"claude","updatedAt":0},{"id":"codex","name":"Codex","windows":[],"source":"sessions","updatedAt":0}]}"#
    let usage = try JSONDecoder().decode(Usage.self, from: Data(json.utf8))
    #expect(PlanMeters(usage).accounts.map(\.title) == ["Claude", "Codex"])
    #expect(PlanMeters(Usage()).accounts.isEmpty)
}
