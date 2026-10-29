import Foundation
import Testing
@testable import KithKit

private func entry(_ json: String) throws -> Entry {
    let data = try JSONDecoder().decode(EntryData.self, from: Data(json.utf8))
    return Entry(id: "e", seq: 1, botId: "b", rev: 1, kind: "permission", turn: 1, data: data, createdAt: 0, updatedAt: 0)
}

private let options = #""options":[{"optionId":"a","name":"Allow","kind":"allow_once"},{"optionId":"b","name":"Always","kind":"allow_always"},{"optionId":"c","name":"Deny","kind":"reject_once"},{"optionId":"d","name":"Never","kind":"reject_always"}]"#

@Test func foldWordsFollowTheChosenOption() throws {
    for (id, text, ok) in [("a", "Allowed npm test", true), ("b", "Always allowed npm test", true),
                           ("c", "Denied npm test", false), ("d", "Never allowed npm test", false)] {
        let e = try entry(#"{"status":"answered","title":"npm test","selected":"\#(id)",\#(options)}"#)
        #expect(CardFold.of(e) == CardFold(ok: ok, text: text))
    }
}

@Test func pendingAndHeldCardsDoNotFold() throws {
    #expect(CardFold.of(try entry(#"{"status":"pending","title":"x",\#(options)}"#)) == nil)
    let held = try entry(#"{"status":"answered","title":"x","selected":"a",\#(options),"held":{"session":"s","kind":"terminal","pid":1,"since":"2026-01-01T00:00:00Z"}}"#)
    #expect(held.data.held != nil)
    #expect(CardFold.of(held) == nil)
}

@Test func expiredAndCancelledFold() throws {
    #expect(CardFold.of(try entry(#"{"status":"expired","title":"npm test"}"#)) == CardFold(ok: false, text: "Expired · npm test"))
    #expect(CardFold.of(try entry(#"{"status":"cancelled"}"#)) == CardFold(ok: false, text: "Cancelled"))
}

@Test func keyAndAccessCardsFold() throws {
    let saved = try entry(#"{"card":"key","secret":{"service":"github","vars":[],"state":"saved","profile":"github/work"}}"#)
    #expect(CardFold.of(saved) == CardFold(ok: true, text: "Saved the key for github/work"))
    let access = try entry(#"{"card":"account","secret":{"service":"github","vars":[],"state":"saved","account":"kai"}}"#)
    #expect(CardFold.of(access) == CardFold(ok: true, text: "Allowed github · kai"))
    let denied = try entry(#"{"card":"key","secret":{"service":"github","vars":[],"state":"denied"}}"#)
    #expect(CardFold.of(denied) == CardFold(ok: false, text: "Not now · github key"))
    let pending = try entry(#"{"card":"key","secret":{"service":"github","vars":[]}}"#)
    #expect(CardFold.of(pending) == nil)
}

@Test func reactionsGroupWithCounts() {
    let groups = ReactionGroups.of(["👍", "❤️", "👍"])
    #expect(groups.map(\.emoji) == ["👍", "❤️"])
    #expect(groups.map(\.count) == [2, 1])
    #expect(ReactionGroups.of([]).isEmpty)
}

@Test func contextLine() {
    #expect(ContextLine.label(percent: 31) == "31% context")
    #expect(ContextLine.label(percent: nil) == nil)
    #expect(ContextLine.fraction(150) == 1)
    #expect(ContextLine.fraction(25) == 0.25)
}
