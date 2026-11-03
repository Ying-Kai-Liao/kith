import Foundation
import Testing
@testable import KithKit

private func entry(_ data: String) throws -> Entry {
    let json = #"{"id":"e1","botId":"b","kind":"permission","seq":1,"rev":1,"turn":1,"createdAt":1,"updatedAt":1,"data":\#(data)}"#
    return try JSONDecoder().decode(Entry.self, from: Data(json.utf8))
}

private let held = HeldRun(session: "s1", kind: "terminal", pid: 4812, since: "2026-10-24T09:40:00Z")
private let calendar: Calendar = {
    var c = Calendar(identifier: .gregorian)
    c.timeZone = TimeZone(identifier: "UTC")!
    return c
}()

@Test func anAnsweredCardDecodesHeld() throws {
    let e = try entry(#"{"status":"answered","held":{"session":"s1","kind":"terminal","pid":4812,"since":"2026-10-24T09:40:00+00:00"}}"#)
    #expect(e.data.held?.kind == "terminal" && e.data.held?.pid == 4812)
    #expect(e.isHeldPermission)
    #expect(try JSONDecoder().decode(EntryData.self, from: JSONEncoder().encode(e.data)) == e.data)
}

@Test func entriesWithoutHeldOrWithNullHeldAreUnchanged() throws {
    #expect(try entry(#"{"status":"answered"}"#).data.held == nil)
    let cleared = try entry(#"{"status":"answered","held":null}"#)
    #expect(cleared.data.held == nil && !cleared.isHeldPermission)
}

@Test func aHeldRowNeverMakesAPendingCardOrANeedsYou() throws {
    let pending = try entry(#"{"status":"pending","held":{"session":"s1","kind":"bot","pid":1,"since":"x"}}"#)
    #expect(!pending.isHeldPermission)
    #expect(Tasks.label(.done, replies: [pending]) == "Done")
    let answered = try entry(#"{"status":"answered","held":{"session":"s1","kind":"bot","pid":1,"since":"x"}}"#)
    #expect(Tasks.label(.done, replies: [answered]) == "Waiting for an old run")
}

@Test func onlyTheOwnerSeesStop() {
    let owner = HeldRunModel(held: held, mac: "Mac mini", owner: true)
    let member = HeldRunModel(held: held, mac: "Mac mini", owner: false, ownerName: "Kai")
    #expect(owner.canStop && !member.canStop)
    #expect(owner.sentence.hasSuffix("It goes on when that ends."))
    #expect(member.sentence.hasSuffix("It goes on when that ends, or when Kai stops it."))
    #expect(HeldRunModel(held: held, mac: "m", owner: false).sentence.hasSuffix("or when the owner stops it."))
}

@Test func theCopyFollowsTheKind() {
    func title(_ kind: String) -> String { HeldRunModel(held: HeldRun(session: "s", kind: kind, pid: 1, since: ""), mac: "m", owner: true).title }
    #expect(title("terminal") == "An old terminal run still has this session")
    #expect(title("oneshot") == "An old background run still has this session")
    #expect(title("bot") == "An old chat run still has this session")
    #expect(title("app") == "An old app turn still has this session")
    #expect(HeldRunModel.symbol("oneshot") == "gearshape.2" && HeldRunModel.symbol("app") == "macwindow")
}

@Test func theConfirmNamesTheMacAndPid() {
    let m = HeldRunModel(held: held, mac: "Mac mini", owner: true)
    #expect(m.confirmTitle == "Stop the old run?")
    let text = m.confirmMessage(agent: "coder")
    #expect(text.hasPrefix("A terminal session on Mac mini has had this session since "))
    #expect(text.contains("(pid 4812) so coder can go on"))
}

@Test func statesChangeTheSentenceAndButton() {
    var m = HeldRunModel(held: held, mac: "Mac mini", owner: true, phase: .stopping)
    #expect(m.sentence == "Stopping it…" && m.canStop)
    m.phase = HeldRunModel.failure(for: HostError.unreachable, mac: "Mac mini")
    #expect(m.buttonTitle == "Try again" && m.canStop && m.isFailed)
    #expect(m.sentence == "Couldn't stop it. Try again, or close it on Mac mini.")
    m.phase = HeldRunModel.failure(for: HostError.http(403, "no"), mac: "Mac mini")
    #expect(!m.canStop && m.sentence.contains("Only the owner can stop it."))
    m.phase = HeldRunModel.ownTurnPhase
    #expect(!m.canStop)
}

@Test func sinceReadsAsAClockThenYesterdayThenADate() {
    let iso = ISO8601DateFormatter()
    let now = iso.date(from: "2026-10-24T12:00:00Z")!
    #expect(held.sinceText(now: now, calendar: calendar) == "9:40")
    #expect(HeldRun(session: "s", kind: "bot", pid: 1, since: "2026-10-23T21:10:00Z").sinceText(now: now, calendar: calendar) == "yesterday 21:10")
    #expect(HeldRun(session: "s", kind: "bot", pid: 1, since: "2026-10-01T08:05:00.123Z").sinceText(now: now, calendar: calendar) == "Oct 1")
    #expect(HeldRunModel.stoppedDivider(kind: "terminal", at: iso.date(from: "2026-10-24T10:02:00Z")!, calendar: calendar) == "Stopped the old terminal run · 10:02")
}

@Test func stopHolderResultDecodes() throws {
    let stopped = try JSONDecoder().decode(StopHolderResult.self, from: Data(#"{"stopped":{"kind":"terminal","pid":4812,"since":"x"}}"#.utf8))
    #expect(stopped.stopped?.pid == 4812 && stopped.ownTurn == nil)
    let gone = try JSONDecoder().decode(StopHolderResult.self, from: Data(#"{"stopped":null}"#.utf8))
    #expect(gone.stopped == nil && gone.ownTurn == nil)
    let own = try JSONDecoder().decode(StopHolderResult.self, from: Data(#"{"stopped":null,"ownTurn":{"kind":"bot","pid":9,"since":"x"}}"#.utf8))
    #expect(own.ownTurn?.kind == "bot")
    let ending = try JSONDecoder().decode(StopHolderResult.self, from: Data(#"{"stopped":null,"stillEnding":{"kind":"oneshot","pid":7,"since":"x"}}"#.utf8))
    #expect(ending.stillEnding?.pid == 7 && ending.stopped == nil && ending.cantStop == nil)
    let cant = try JSONDecoder().decode(StopHolderResult.self, from: Data(#"{"stopped":null,"cantStop":{"kind":"terminal","pid":8,"since":"x"}}"#.utf8))
    #expect(cant.cantStop?.pid == 8)
}
