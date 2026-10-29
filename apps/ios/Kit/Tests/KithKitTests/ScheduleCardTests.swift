import Foundation
import Testing
@testable import KithKit

/// Thursday 29 Oct 2026, local time.
private func at(_ day: Int, _ h: Int, _ m: Int) -> Date {
    Calendar.current.date(from: DateComponents(year: 2026, month: 10, day: day, hour: h, minute: m))!
}
private let now = at(29, 9, 20)
private func ms(_ d: Date) -> Int64 { Int64(d.timeIntervalSince1970 * 1000) }

private func job(_ extra: [String: RichCard.JSON] = [:]) -> RichCard {
    let snapshot: [String: RichCard.JSON] = [
        "name": .string("Daily news"), "op": .string("added"), "at": .int(ms(at(29, 9, 12))), "person": .null,
        "cron": .string("43 7 * * *"), "every": .string("Every day at 7:43"),
        "task": .string("Daily news brief: read /Users/kai/hq/agents/personal/role/skills/learned/daily-news/SKILL.md and follow it."),
        "paused": .bool(false), "nextRun": .string(ISO8601DateFormatter().string(from: at(30, 7, 43))), "lastRun": .null, "lastStatus": .null,
    ]
    return RichCard(type: "schedule", v: 1, ref: ["agent": .string("personal"), "id": .string("daily-news")],
                    snapshot: snapshot.merging(extra) { $1 })
}

private func view(_ card: RichCard, postedAt: Date = at(29, 9, 12), live: ScheduleLive?? = .none,
                  viewerIsPerson: Bool = true, personName: String? = nil, opened: Bool = false) -> ScheduleCardModel {
    ScheduleCardModel(card: card, postedAt: postedAt, now: now, live: live, viewerIsPerson: viewerIsPerson,
                      personName: personName, opened: opened)
}

private func decode<T: Decodable>(_ type: T.Type, _ json: String) throws -> T {
    try JSONDecoder().decode(T.self, from: Data(json.utf8))
}

@Test func aNewJobReadsFromItsSnapshot() {
    let v = view(job())
    #expect(v.kind == .job)
    #expect(v.op == "Added · 9:12")
    #expect(v.every == "Every day at 7:43")
    #expect(v.next == "Tomorrow 7:43")
    #expect(v.last == .init(text: "Not run yet", running: false, failed: false, error: nil))
    #expect(v.runsAs == "you")
    #expect(v.task == "Daily news brief: read …/learned/daily-news/SKILL.md and follow it.")
    #expect(v.fold == nil)
}

@Test func liveStateWinsOverTheSnapshot() {
    let running = view(job(), live: ScheduleLive(paused: false, nextRun: at(30, 7, 43), last: .init(running: true, at: at(29, 7, 43))))
    #expect(running.last == .init(text: "Running since 7:43", running: true, failed: false, error: nil))
    let paused = view(job(), live: ScheduleLive(paused: true, nextRun: nil, last: .init(running: false, at: at(29, 7, 43))))
    #expect(paused.paused)
    #expect(paused.op == nil)
    #expect(paused.next == "Paused")
    #expect(paused.last.text == "Today 7:43")
}

@Test func aJobSomeoneElseRunsAsNamesThem() {
    let card = job(["person": .string("mia")])
    #expect(view(card, viewerIsPerson: false, personName: "Mia").runsAs == "Mia")
    #expect(view(card, viewerIsPerson: true, personName: "Mia").runsAs == "you")
}

@Test func aChangedOrRemovedCardIsALine() {
    #expect(view(job(["folded": .string("changed")])).fold?.text == "Daily news · changed below")
    #expect(view(job(["folded": .string("removed")])).fold?.text == "Daily news · removed below")
    #expect(view(job(["op": .string("removed")])).fold?.text == "Daily news · removed by you")
    let gone = view(job(), live: .some(nil))
    #expect(gone.fold?.text == "Daily news · removed by you")
    #expect(gone.fold?.opens == false)
    #expect(gone.next == "Tomorrow 7:43")
}

@Test func afterItsDayTheCardIsALivePillThatOpens() {
    let live = ScheduleLive(paused: false, nextRun: at(29, 9, 43), last: nil)
    let old = view(job(), postedAt: at(27, 9, 12), live: live)
    #expect(old.fold == .init(text: "Daily news · every day 7:43 · next 9:43", trailing: "Tue", opens: true, done: false))
    #expect(view(job(), postedAt: at(27, 9, 12), live: live, opened: true).fold == nil)
    var stopped = live
    stopped.paused = true
    #expect(view(job(), postedAt: at(27, 9, 12), live: stopped).fold?.text == "Daily news · every day 7:43 · paused")
}

@Test func aRoutineRunsAsItsBotAndShowsAFailedRun() throws {
    let card = RichCard(type: "schedule", v: 1, ref: ["bot": .string("b1"), "routine": .string("r1")], snapshot: [
        "name": .string("Issue watch"), "op": .string("created"), "every": .string("Every 30 minutes"),
        "task": .string("Check issues"), "paused": .bool(false),
    ])
    let routine = #"{"id":"r1","botId":"b1","name":"Issue watch","instruction":"Check issues","triggers":[],"triggerDescriptions":[],"enabled":true,"createdAt":1,"updatedAt":1,"nextRunAt":\#(ms(at(29, 11, 32))),"lastError":"Timed out after 10 min"}"#
    let runs = #"[{"id":"x2","routineId":"r1","botId":"b1","status":"failed","createdAt":\#(ms(at(29, 8, 52))),"finishedAt":\#(ms(at(29, 9, 2)))},{"id":"x1","routineId":"r1","botId":"b1","status":"done","createdAt":\#(ms(at(29, 8, 22)))}]"#
    let listing = try decode(RoutineListing.self, #"{"routines":[\#(routine)],"runs":\#(runs)}"#)
    let v = view(card, live: ScheduleLive.routine(listing, id: "r1"))
    #expect(v.kind == .routine)
    #expect(v.runsAs == nil)
    #expect(v.op == "Created · 9:12")
    #expect(v.last == .init(text: "Today 9:02", running: false, failed: true, error: "Timed out after 10 min"))
    #expect(v.next == "Today 11:32")
    #expect(ScheduleLive.routine(try decode(RoutineListing.self, #"{"routines":[],"runs":[]}"#), id: "r1") == nil)
}

@Test func aKithJobReadLiveFromKithInfoSchedules() throws {
    let schedules = try decode([KithSchedule].self, #"[{"id":"daily-news","cron":"43 7 * * *","task":"","paused":false,"last_run":\#(ms(at(29, 7, 43))),"last_status":"running","next_run":\#(ms(at(30, 7, 43)))}]"#)
    #expect(ScheduleLive.kith(schedules, id: "daily-news") == ScheduleLive(paused: false, nextRun: at(30, 7, 43), last: .init(running: true, at: at(29, 7, 43))))
    #expect(ScheduleLive.kith(schedules, id: "other") == nil)
}

@Test func aSnapshotMissingPiecesStillDraws() {
    let v = view(RichCard(type: "schedule", v: 1))
    #expect(v.name == "Schedule")
    #expect(v.next == "—")
    #expect(v.last.text == "Not run yet")
}

@Test func dayWordsQuietScheduleWordsAndShortPaths() {
    #expect(CardWords.day(at(30, 1, 0), now: now) == "Tomorrow")
    #expect(CardWords.day(at(31, 1, 0), now: now) == "Sat")
    #expect(CardWords.day(at(28, 1, 0), now: now) == "Yesterday")
    #expect(CardWords.day(at(5, 1, 0), now: now) == "5 Oct")
    #expect(CardWords.quietEvery("Weekdays at 7:30") == "weekdays 7:30")
    #expect(CardWords.quietEvery("Every 30 minutes") == "every 30 minutes")
    #expect(CardWords.shortPaths("read ~/a/b/c/d.md now") == "read …/b/c/d.md now")
    #expect(CardWords.shortPaths("read /b/c/d.md") == "read /b/c/d.md")
}
