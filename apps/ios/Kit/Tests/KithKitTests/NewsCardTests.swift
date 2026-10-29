import Foundation
import Testing
@testable import KithKit

private func at(_ day: Int, _ h: Int, _ m: Int) -> Date {
    Calendar.current.date(from: DateComponents(year: 2026, month: 10, day: day, hour: h, minute: m))!
}

private func row(_ n: Int) -> RichCard.JSON {
    .object(["title": .string("Item \(n)"), "line": .string("One line."), "source": n == 1 ? .string("BOM") : .null,
             "domain": .string("bom.gov.au"), "url": .string("https://bom.gov.au/\(n)")])
}

private func card(_ rows: [RichCard.JSON]) -> RichCard {
    RichCard(type: "news", v: 1, ref: ["agent": .string("personal"), "job": .string("morning-brief")], snapshot: [
        "name": .string("Morning brief"), "every": .string("Every day at 7:00"), "lead": .string("Five worth your time today."),
        "tail": .string("Want the long read summarised?"), "rows": .array(rows),
    ])
}

@Test func theDayItCameShowsTheHeaderThreeRowsAndShowMore() {
    let v = NewsCardModel(card: card((1...5).map(row)), postedAt: at(25, 7, 0), now: at(25, 9, 0))
    #expect(v.title == "Morning brief · Sun 25 Oct")
    #expect(v.every == "every day 7:00")
    #expect(v.lead == "Five worth your time today.")
    #expect(v.tail == "Want the long read summarised?")
    #expect(v.shown.map(\.title) == ["Item 1", "Item 2", "Item 3"])
    #expect(v.shown.first?.source == "BOM" && v.shown.last?.source == nil)
    #expect(v.more == 2)
    #expect(v.fold == nil)
    #expect(NewsCardModel(card: card((1...5).map(row)), postedAt: at(25, 7, 0), now: at(25, 9, 0), expanded: true).more == 0)
}

@Test func afterItsDayItFoldsAndOpenedShowsEveryItem() {
    let rows = (1...6).map(row)
    #expect(NewsCardModel(card: card(rows), postedAt: at(24, 7, 0), now: at(25, 9, 0)).fold == "Morning brief · Sat · 6 items")
    let opened = NewsCardModel(card: card(rows), postedAt: at(24, 7, 0), now: at(25, 9, 0), opened: true)
    #expect(opened.fold == nil)
    #expect(opened.shown.count == 6)
}

@Test func rowsWithoutATitleOrAWebLinkAreLeftOut() {
    let bad: [RichCard.JSON] = [row(1), .object(["title": .string("x"), "url": .string("javascript:alert(1)")]),
                                .object(["url": .string("https://a.b")]), .string("no")]
    let v = NewsCardModel(card: RichCard(type: "news", v: 1, snapshot: ["name": .string("Brief"), "rows": .array(bad)]),
                          postedAt: at(25, 7, 0), now: at(25, 8, 0))
    #expect(v.rows.map(\.title) == ["Item 1"])
    #expect(v.every == nil)
}
