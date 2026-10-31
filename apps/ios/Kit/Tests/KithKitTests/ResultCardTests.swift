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

@Test func thisAppDrawsTheResultCardOfVersionsOneAndTwo() throws {
    #expect(RichCard(type: "result", v: 1).isDrawable())
    #expect(RichCard(type: "result", v: 2).isDrawable())
    #expect(!RichCard(type: "result", v: 3).isDrawable())
}

private func card2(_ snapshot: String) throws -> RichCard {
    let json = #"{"id":"e1","botId":"b","kind":"agent","seq":1,"rev":1,"turn":1,"createdAt":1,"updatedAt":1,"data":{"text":"the reply","final":true,"richCard":{"type":"result","v":2,"snapshot":\#(snapshot)}}}"#
    return try #require(JSONDecoder().decode(Entry.self, from: Data(json.utf8)).data.richCard)
}

@Test func aVersionTwoCardHasTitleSubRowsAndTheRepliesLink() throws {
    let model = try #require(ResultCardModel(card: card2(#"{"title":"QF 432","sub":"On time so far.","rows":[{"label":"Departs","value":"5:35 pm"},{"label":"Lands","value":"7:05 pm"}],"links":[{"label":"qantas.com/flight-status","url":"https://qantas.com/flight-status"}]}"#)))
    #expect(model.title == "QF 432")
    #expect(model.sub == "On time so far.")
    #expect(model.rows == [.init(label: "Departs", value: "5:35 pm"), .init(label: "Lands", value: "7:05 pm")])
    #expect(model.facts.isEmpty)
    #expect(model.links.map(\.label) == ["qantas.com/flight-status"])
}

@Test func aVersionTwoCardNeedsATitleAndARowAndTheSubIsOptional() throws {
    #expect(try ResultCardModel(card: card2(#"{"rows":[{"label":"a","value":"b"}]}"#)) == nil)
    #expect(try ResultCardModel(card: card2(#"{"title":"T"}"#)) == nil)
    #expect(try ResultCardModel(card: card2(#"{"title":"T","rows":[{"label":"a","value":" "},"nope",3]}"#)) == nil)
    #expect(try ResultCardModel(card: card2(#"{"title":"T","facts":["a"]}"#)) == nil)
    #expect(ResultCardModel(card: RichCard(type: "result", v: 2)) == nil)
    #expect(try ResultCardModel(card: card2(#"{"title":"T","rows":[{"label":"a","value":"b"}]}"#))?.sub == "")
}

@Test func aVersionTwoRowWithoutALabelKeepsItsValueAndOnlyFiveRowsShow() throws {
    let rows = (1...6).map { #"{"value":"\#($0)"}"# }.joined(separator: ",")
    let model = try #require(ResultCardModel(card: card2(#"{"title":"T","rows":[\#(rows)]}"#)))
    #expect(model.rows.count == 5)
    #expect(model.rows.first == .init(label: "", value: "1"))
}

@Test func versionTwoLinksAreWebOnlyAtMostTwoAndAnUnlabelledOneShowsItsAddress() throws {
    let model = try #require(ResultCardModel(card: card2(#"{"title":"T","rows":[{"label":"a","value":"b"}],"links":[{"label":"x","url":"javascript:alert(1)"},{"url":"https://a.example/1"},{"label":"B","url":"http://b.example"},{"label":"C","url":"https://c.example"}]}"#)))
    #expect(model.links.map(\.label) == ["a.example/1", "B"])
}
