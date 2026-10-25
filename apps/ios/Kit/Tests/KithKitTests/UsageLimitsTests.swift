import Foundation
import Testing
@testable import KithKit

private func entry(_ data: String) throws -> Entry {
    let json = #"{"id":"p1","botId":"architect","kind":"permission","seq":1,"rev":1,"turn":1,"createdAt":1,"updatedAt":1,"data":\#(data)}"#
    return try JSONDecoder().decode(Entry.self, from: Data(json.utf8))
}

private let agentCard = #"{"status":"pending","toolKind":"limit","agent":"architect","title":"This run is waiting: architect has used its $300 for October.","options":[{"optionId":"allow","name":"Allow this run","kind":"limit"},{"optionId":"raise","name":"Raise to $400","kind":"limit"},{"optionId":"stop","name":"Stop","kind":"limit"}],"limit":{"kind":"agent","agent":"architect","spent":300.4,"limit":300,"at":"ask","month":"October","perRun":2.92,"raiseTo":400}}"#

private let personCard = #"{"status":"pending","toolKind":"limit","approver":"owner","personName":"Mia","options":[{"optionId":"add","name":"Add $10","kind":"limit"},{"optionId":"raise","name":"Raise to $40","kind":"limit"},{"optionId":"leave","name":"Leave it","kind":"limit"}],"limit":{"kind":"person","agent":"video","person":"mia","spent":20.1,"limit":20,"month":"October","addTo":30,"raiseTo":40}}"#

/// 2026-10-26 14:02 in a fixed zone.
private let calendar: Calendar = {
    var c = Calendar(identifier: .gregorian)
    c.timeZone = TimeZone(identifier: "Australia/Sydney")!
    return c
}()
private let now = calendar.date(from: DateComponents(year: 2026, month: 10, day: 26, hour: 14, minute: 2))!

@Test func moneyIsWholeDollarsFromTenAndCentsBelowOrWhenAsked() {
    #expect(Spend.money(6667.2) == "$6,667")
    #expect(Spend.money(2.92) == "$2.92")
    #expect(Spend.money(289.01, cents: true) == "$289.01")
    #expect(Spend.of(300.4, 300, cents: true) == "$300.40 of $300")
    #expect(Spend.runs(99, perRun: 2.92) == "99 runs · $2.92 a run")
    #expect(Spend.fullness(450, 300) == 1 && Spend.fullness(10, nil) == 0)
}

@Test func theUsageReportReadsNoLimitAsNil() throws {
    let json = #"{"period":"month","label":"October","since":"2026-10-01","prices":"api","total":6667,"agents":910,"own":5757,"rows":[{"agent":"architect","cost":289.01,"runs":99,"perRun":2.92,"limit":300,"at":"ask","month":289.01},{"agent":"coder","cost":12,"runs":4,"perRun":3,"limit":null,"at":null,"month":null}]}"#
    let r = try JSONDecoder().decode(UsageReport.self, from: Data(json.utf8))
    #expect(r.label == "October" && r.own == 5757)
    #expect(r.rows[0].limit == 300 && r.rows[0].at == .ask)
    #expect(r.rows[1].limit == nil && r.rows[1].month == nil && r.rows[1].at == .ask)
}

@Test func anAgentsMonthDecodesWithItsForecast() throws {
    let json = #"{"agent":"architect","period":"month","label":"October","cost":289.01,"runs":99,"perRun":2.92,"month":289.01,"monthLabel":"October","limit":300,"at":"tell","raiseTo":400,"days":[{"day":"2026-10-25","cost":72}],"latest":null,"models":[{"model":"claude-opus-5-5","effort":"high","cost":280,"runs":90}],"costliest":[{"session":"s1","cost":9.5,"started":"2026-10-24 00:04:08","via":"chat","turn":"Draft the plan\nwith details"}],"forecast":{"perDay":72,"since":"2026-10-23","text":"","monthEnd":2200,"reach":{"300":"2026-10-26","500":null}}}"#
    let v = try JSONDecoder().decode(AgentUsage.self, from: Data(json.utf8))
    #expect(v.at == .tell && v.limit == 300 && v.days.count == 1)
    #expect(v.costliest.first?.title == "Draft the plan")
    #expect(v.forecast?.reach.keys.contains("500") == true && v.forecast?.reach["300"] == "2026-10-26")
    #expect(Spend.agentForecast(v, now: now, calendar: calendar) == "About $72 a day since Oct 23, so it reaches $300 today.")
    #expect(Spend.modelLine(v.models) == "Opus 5.5 · high")
    #expect(Spend.modelLine([]) == "—")
}

@Test func theLimitSheetsForecastSaysWhenEachLimitRunsOut() {
    let f = SpendForecast(perDay: 72, since: "2026-10-23", text: "", reach: ["300": "2026-10-26", "500": "2026-10-29"])
    #expect(Spend.forecast(f, spent: 289.01, chosen: 300, now: now, calendar: calendar) == "At about $72 a day, $300 runs out today and $500 lasts to about Oct 29.")
    #expect(Spend.forecast(f, spent: 289.01, chosen: nil, now: now, calendar: calendar) == "At about $72 a day, no limit: runs never wait for one.")
    #expect(Spend.forecast(nil, spent: 0, chosen: nil, now: now, calendar: calendar) == "No limit: runs never wait for one.")
    #expect(Spend.nextMonthStart(now: now, calendar: calendar) == "Nov 1")
}

@Test func anAgentsLimitCardSaysItsMessageIsKept() throws {
    let e = try entry(agentCard)
    let m = try #require(LimitCardModel(e))
    #expect(m.heading == "architect · Monthly limit")
    #expect(m.left == "About $3 a run" && m.right == "$300.40 of $300" && m.fullness == 1)
    #expect(m.options.map(\.id) == ["allow", "raise", "stop"])
    #expect(m.options.map(\.tone) == [.ink, .pair, .plain])
    #expect(m.footer == "Your message is kept. Nothing runs until you answer.")
    let board = try #require(LimitCardModel(e, board: true))
    #expect(board.title == "architect used its $300 for October. One run is waiting." && board.footer == nil)
    #expect(Spend.question(e) == board.title)
}

@Test func aPersonsLimitCardNamesThemAndOffersMore() throws {
    let e = try entry(personCard)
    let m = try #require(LimitCardModel(e))
    #expect(m.heading == "Mia · limit on video")
    #expect(m.options.map(\.label) == ["Add $10", "Raise to $40", "Leave it"])
    #expect(m.footer == nil)
    #expect(Spend.question(e) == "Mia reached their $20 for October.")
}

@Test func anAnsweredLimitCardSaysWhatWasChosen() throws {
    #expect(Spend.outcome(try entry(agentCard.replacingOccurrences(of: #""status":"pending""#, with: #""status":"answered","selected":"raise""#))) == "Raised to $400")
    #expect(Spend.outcome(try entry(agentCard.replacingOccurrences(of: #""status":"pending""#, with: #""status":"answered","selected":"stop""#))) == "Stopped · your message is kept")
    #expect(Spend.outcome(try entry(personCard.replacingOccurrences(of: #""status":"pending""#, with: #""status":"answered","selected":"add""#))) == "Added $10")
    #expect(LimitCardModel(try entry(#"{"status":"pending","title":"swift test"}"#)) == nil)
}

@Test func peoplesSpendReadsTheirBudgets() throws {
    let json = #"{"agent":"video","month":"2026-10","people":{"owner":12.5,"mia":20.1},"budgets":{"mia":20,"sam":null}}"#
    let p = try JSONDecoder().decode(AgentPeople.self, from: Data(json.utf8))
    #expect(p.line(for: "mia") == "$20.10 of $20")
    #expect(p.line(for: "sam") == "$0.00 this month")
    #expect(p.line(for: "owner") == "$12.50 this month")
}
