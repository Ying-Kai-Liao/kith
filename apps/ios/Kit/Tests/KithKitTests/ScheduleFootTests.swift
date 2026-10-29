import Foundation
import Testing
@testable import KithKit

private func at(_ h: Int, _ m: Int) -> Date {
    Calendar.current.date(from: DateComponents(year: 2026, month: 10, day: 29, hour: h, minute: m))!
}
private func ms(_ d: Date) -> Int64 { Int64(d.timeIntervalSince1970 * 1000) }

private func view(_ extra: [String: RichCard.JSON] = [:]) -> ScheduleCardModel {
    let snapshot: [String: RichCard.JSON] = [
        "name": .string("Daily news"), "op": .string("added"), "every": .string("Every day at 7:43"),
        "paused": .bool(false), "lastRun": .null, "lastStatus": .null,
    ]
    let card = RichCard(type: "schedule", v: 1, ref: ["agent": .string("personal"), "id": .string("daily-news")],
                        snapshot: snapshot.merging(extra) { $1 })
    return ScheduleCardModel(card: card, postedAt: at(9, 12), now: at(9, 20), viewerIsPerson: true, personName: nil)
}

private func foot(_ v: ScheduleCardModel, owner: Bool = true, ownerName: String? = nil, confirming: Bool = false,
                  busy: ScheduleOp? = nil) -> ScheduleFoot {
    ScheduleFoot(v, owner: owner, ownerName: ownerName, confirming: confirming, busy: busy)
}

@Test func theOwnerGetsPauseAndRunNowAndResumeInInkWhenPaused() {
    #expect(foot(view()) == .buttons(toggle: .init(op: .pause, label: "Pause", ink: false), run: "Run now", runDisabled: false, busy: false))
    #expect(foot(view(["paused": .bool(true)])) == .buttons(toggle: .init(op: .resume, label: "Resume", ink: true), run: "Run now", runDisabled: false, busy: false))
}

@Test func runNowReadsRunningUntilTheRunEndsAndOneCallGoesAtATime() {
    guard case let .buttons(_, run, disabled, _) = foot(view(), busy: .run) else { Issue.record("no buttons"); return }
    #expect(run == "Running…" && disabled)
    let running = view(["lastRun": .int(ms(at(9, 15))), "lastStatus": .string("running")])
    guard case let .buttons(_, run2, disabled2, _) = foot(running) else { Issue.record("no buttons"); return }
    #expect(run2 == "Running…" && disabled2)
    guard case let .buttons(_, _, disabled3, busy) = foot(view(), busy: .pause) else { Issue.record("no buttons"); return }
    #expect(disabled3 && busy)
}

@Test func theTrashAsksInsideTheCard() {
    #expect(foot(view(), confirming: true) == .confirm(question: "Remove Daily news? It won't run again, and its task goes with it.", removing: false))
    #expect(foot(view(), confirming: true, busy: .remove) == .confirm(question: "Remove Daily news? It won't run again, and its task goes with it.", removing: true))
}

@Test func othersReadWhoCanPauseOrRunIt() {
    #expect(foot(view(), owner: false, ownerName: "Kai") == .note("Kai can pause or run it."))
    #expect(foot(view(), owner: false, confirming: true) == .note("The owner can pause or run it."))
    #expect(ScheduleFoot.ownerName(from: "Waiting for Kai") == "Kai")
    #expect(ScheduleFoot.ownerName(from: "Working") == nil)
    #expect(ScheduleFoot.ownerName(from: nil) == nil)
}
