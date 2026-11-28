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

private func harness(_ json: String) throws -> [HarnessAccount] {
    try JSONDecoder().decode([HarnessAccount].self, from: Data(json.utf8))
}

@Test func planMetersPerAccountUseTheirOwnWindows() throws {
    let usage = try JSONDecoder().decode(Usage.self, from: Data(#"{"providers":[{"id":"claude","name":"Claude","windows":[{"id":"w","label":"old","percent":1}],"source":"claude","updatedAt":0}]}"#.utf8))
    let accounts = try harness(#"""
    [{"id":"claude","usage":{"readAt":1000000,"windows":[{"name":"five_hour","percent":40,"resetsAt":2000000},{"name":"seven_day","percent":10}]}},
     {"id":"claude:school","usage":{"readAt":900000,"windows":[{"name":"five_hour","percent":90}]}}]
    """#)
    let meters = PlanMeters(usage, accounts: accounts)
    #expect(meters.accounts.map(\.title) == ["Claude", "Claude · school"])
    #expect(meters.accounts[0].provider.windows.map(\.label) == ["5-hour", "Weekly"])
    #expect(meters.accounts[0].provider.windows[0].resetsAt == 2_000_000)
    #expect(meters.accounts[0].readAt == Date(timeIntervalSince1970: 1000))
    #expect(meters.accounts[1].provider.windows[0].percent == 90)
}

@Test func planMetersFallBackToTheProviderWhenAnAccountHasNoUsage() throws {
    let usage = try JSONDecoder().decode(Usage.self, from: Data(#"{"providers":[{"id":"codex","name":"Codex","windows":[{"id":"p","label":"5-hour","percent":27}],"source":"sessions","updatedAt":0}]}"#.utf8))
    let accounts = try harness(#"[{"id":"codex","status":"signed_out","usage":null}]"#)
    let meters = PlanMeters(usage, accounts: accounts)
    #expect(meters.accounts.map(\.title) == ["Codex"])
    #expect(meters.accounts[0].readAt == nil)
    // A host before 0.1 answers no usage field at all.
    #expect(PlanMeters(usage, accounts: try harness(#"[{"id":"codex"}]"#)).accounts.map(\.title) == ["Codex"])
    #expect(PlanMeters(usage, accounts: nil).accounts.map(\.title) == ["Codex"])
}

@Test func planMetersReadLabel() {
    let now = Date(timeIntervalSince1970: 100_000)
    #expect(PlanMeters.readLabel(now.addingTimeInterval(-20), now: now) == "just now")
    #expect(PlanMeters.readLabel(now.addingTimeInterval(-300), now: now) == "5m ago")
    #expect(PlanMeters.readLabel(now.addingTimeInterval(-7200), now: now) == "2h ago")
}
