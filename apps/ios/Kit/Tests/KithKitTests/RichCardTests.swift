import Foundation
import Testing
@testable import KithKit

private func entry(_ data: String) throws -> Entry {
    let json = #"{"id":"e1","botId":"b","kind":"agent","seq":1,"rev":1,"turn":1,"createdAt":1,"updatedAt":1,"data":\#(data)}"#
    return try JSONDecoder().decode(Entry.self, from: Data(json.utf8))
}

private let known = ["schedule": 2]

@Test func aCardOfAKnownTypeAndVersionIsDrawn() throws {
    let e = try entry(#"{"text":"Every day at 8:00","final":true,"richCard":{"type":"schedule","v":2,"ref":{"agent":"echo","id":"brief"},"snapshot":{"cron":"0 8 * * *"}}}"#)
    let card = try #require(e.data.richCard)
    #expect(card.type == "schedule" && card.v == 2)
    #expect(card.ref?["id"] == .string("brief") && card.snapshot?["cron"] == .string("0 8 * * *"))
    #expect(card.isDrawable(known: known))
    #expect(e.data.text == "Every day at 8:00")
    #expect(try JSONDecoder().decode(EntryData.self, from: JSONEncoder().encode(e.data)) == e.data)
}

@Test func anUnknownTypeOrANewerVersionIsThePlainRow() throws {
    #expect(try entry(#"{"richCard":{"type":"news","v":1}}"#).data.richCard?.isDrawable(known: known) == false)
    #expect(try entry(#"{"richCard":{"type":"schedule","v":3}}"#).data.richCard?.isDrawable(known: known) == false)
    #expect(try entry(#"{"richCard":{"type":"schedule","v":1}}"#).data.richCard?.isDrawable() == true)
    #expect(try entry(#"{"richCard":{"type":"news","v":1}}"#).data.richCard?.isDrawable() == true)
    #expect(try entry(#"{"richCard":{"type":"news","v":2}}"#).data.richCard?.isDrawable() == false)
    #expect(try entry(#"{"richCard":{"type":"weather","v":1}}"#).data.richCard?.isDrawable() == false)
}

@Test func anythingThatIsNotACardStillDecodesTheEntry() throws {
    for bad in [#""schedule""#, "7", "[]", "{}", #"{"type":"schedule"}"#, #"{"type":"schedule","v":"1"}"#,
                #"{"type":"schedule","v":1.5}"#, #"{"type":"schedule","v":1,"ref":"brief"}"#, #"{"type":7,"v":1}"#] {
        let e = try entry(#"{"text":"the twin","richCard":\#(bad)}"#)
        #expect(e.data.text == "the twin", "\(bad)")
        #expect(e.data.richCard?.isDrawable(known: known) != true, "\(bad)")
    }
    #expect(try entry(#"{"text":"t","richCard":null}"#).data.richCard == nil)
}

@Test func thePermissionCardIsNotARichCard() throws {
    let e = try entry(#"{"card":"key"}"#)
    #expect(e.data.card == "key" && e.data.richCard == nil)
}
