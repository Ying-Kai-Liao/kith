import Foundation
import Testing
@testable import KithKit

@Test func cronReadsAsWords() {
    #expect(RoutineSummary.cron("0 9 * * *") == "Every day at 9:00")
    #expect(RoutineSummary.cron("30 18 * * 1-5") == "Weekdays at 18:30")
    #expect(RoutineSummary.cron("0 * * * *") == "Every hour")
    #expect(RoutineSummary.cron("*/15 * * * *") == "Every 15 minutes")
    #expect(RoutineSummary.cron("0 */2 * * *") == "Every 2 hours")
    #expect(RoutineSummary.cron("0 16 * * 5") == "Every Friday at 16:00")
    #expect(RoutineSummary.cron("5 7 * * 1,3,5") == "Monday, Wednesday and Friday at 7:05")
    #expect(RoutineSummary.cron("0 9 1 * *") == nil)
    #expect(RoutineSummary.cron("nonsense") == nil)
}

private func routine(_ triggers: [RoutineTrigger], described: [String] = [], error: String? = nil) -> Routine {
    Routine(id: "r", botId: "b", name: "n", instruction: "", triggers: triggers, enabled: true, createdAt: 0, updatedAt: 0,
            triggerDescriptions: described, nextRunAt: nil, timeoutSeconds: nil, lastError: error)
}

@Test func scheduleFallsBackToHostWordsThenRawText() {
    #expect(RoutineSummary.schedule(routine([.init(type: "interval", seconds: 3600)])) == "Every hour")
    #expect(RoutineSummary.schedule(routine([.init(type: "cron", expression: "0 9 1 * *")], described: ["Monthly on the 1st"])) == "Monthly on the 1st")
    #expect(RoutineSummary.schedule(routine([.init(type: "cron", expression: "0 9 1 * *")])) == "0 9 1 * *")
    #expect(RoutineSummary.schedule(routine([], described: ["Every day at 7:00"])) == "Every day at 7:00")
}

@Test func lastRanIsRelativeOrNever() {
    let now = Date(timeIntervalSince1970: 1_000_000)
    func run(_ agoSeconds: Double) -> RoutineRun {
        RoutineRun(id: "u", routineId: "r", botId: "b", status: "completed", createdAt: Int64((1_000_000 - agoSeconds) * 1000),
                   finishedAt: nil, detail: nil, rootId: nil)
    }
    #expect(RoutineSummary.lastRan(nil, now: now) == "Never ran")
    #expect(RoutineSummary.lastRan(run(30), now: now) == "Last ran just now")
    #expect(RoutineSummary.lastRan(run(7200), now: now) == "Last ran 2 hours ago")
    #expect(RoutineSummary.lastRan(run(90_000), now: now) == "Last ran yesterday")
    #expect(RoutineSummary.lastRan(run(400_000), now: now) == "Last ran 4 days ago")
}

@Test func attentionNamesTheProblem() {
    let r = routine([], error: "bad schedule")
    #expect(RoutineSummary.attention(r, lastRun: nil) == "Needs attention: bad schedule")
    let failed = RoutineRun(id: "u", routineId: "r", botId: "b", status: "failed", createdAt: 0, finishedAt: nil, detail: "timed out", rootId: nil)
    #expect(RoutineSummary.attention(routine([]), lastRun: failed) == "Needs attention: last run failed: timed out")
    #expect(RoutineSummary.attention(routine([]), lastRun: nil) == nil)
}
