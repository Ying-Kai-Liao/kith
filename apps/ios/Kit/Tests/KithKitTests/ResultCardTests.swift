import Foundation
import Testing
@testable import KithKit

private func card(_ snapshot: String) throws -> RichCard {
    let json = #"{"id":"e1","botId":"b","kind":"agent","seq":1,"rev":1,"turn":1,"createdAt":1,"updatedAt":1,"data":{"text":"the reply","final":true,"richCard":{"type":"result","v":1,"snapshot":\#(snapshot)}}}"#
    return try #require(JSONDecoder().decode(Entry.self, from: Data(json.utf8)).data.richCard)
}

@Test func aResultCardHasItsTitleFactsAndTheRepliesLinks() throws {
    let model = try #require(ResultCardModel(card: card(#"{"title":"Disk check on the Mac mini","facts":["Free space: 340 GB of 500 GB","Memory: 12 GB in use"],"links":[{"label":"Open report","url":"https://example.com/report/1"}]}"#)))
    #expect(model.title == "Disk check on the Mac mini")
    #expect(model.facts == ["Free space: 340 GB of 500 GB", "Memory: 12 GB in use"])
    #expect(model.links.map(\.label) == ["Open report"])
    #expect(model.links.first?.url.absoluteString == "https://example.com/report/1")
}

@Test func aCardWithoutATitleOrAFactIsThePlainReply() throws {
    #expect(try ResultCardModel(card: card(#"{"facts":["a"]}"#)) == nil)
    #expect(try ResultCardModel(card: card(#"{"title":"T"}"#)) == nil)
    #expect(try ResultCardModel(card: card(#"{"title":"T","facts":[" ",3]}"#)) == nil)
    #expect(ResultCardModel(card: RichCard(type: "result", v: 1)) == nil)
}

@Test func onlyWebLinksBecomeButtonsAtMostTwoAndUnlabelledOnesSayOpen() throws {
    let model = try #require(ResultCardModel(card: card(#"{"title":"T","facts":["a"],"links":[{"label":"x","url":"javascript:alert(1)"},{"url":"https://a.example/1"},"nope",{"label":"B","url":"http://b.example"},{"label":"C","url":"https://c.example"}]}"#)))
    #expect(model.links.map(\.label) == ["Open", "B"])
}

@Test func factsAreCappedAtFour() throws {
    let model = try #require(ResultCardModel(card: card(#"{"title":"T","facts":["1","2","3","4","5"]}"#)))
    #expect(model.facts.count == 4)
}

@Test func thisAppDrawsTheResultCardOfVersionOne() throws {
    #expect(RichCard(type: "result", v: 1).isDrawable())
    #expect(!RichCard(type: "result", v: 2).isDrawable())
}
