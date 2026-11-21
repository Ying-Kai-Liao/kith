import Foundation
import Testing
@testable import KithKit

@Test func aFactIsCutIntoPlainAndBoldPiecesInOrder() {
    let fact = CardFact(icon: "terminal", text: "Runs a Python script written into the command, and ffmpeg", strong: ["Python script", "ffmpeg"])
    #expect(fact.pieces == [
        .init(text: "Runs a ", bold: false),
        .init(text: "Python script", bold: true),
        .init(text: " written into the command, and ", bold: false),
        .init(text: "ffmpeg", bold: true),
    ])
    // A repeated word bolds the one the host meant: the second, after the first was passed.
    #expect(CardFact(icon: "folder", text: "Saves into dl dl", strong: ["dl", "dl"]).pieces.filter(\.bold).count == 2)
    #expect(CardFact(icon: "terminal", text: "A shell command: curl", strong: ["A shell command"]).pieces == [
        .init(text: "A shell command", bold: true), .init(text: ": curl", bold: false),
    ])
}

@Test func aFactWithNoBoldPartsOrOneTheTextLacksIsPlain() {
    #expect(CardFact(icon: "terminal", text: "Kith can\u{2019}t read it").pieces == [.init(text: "Kith can\u{2019}t read it", bold: false)])
    #expect(CardFact(icon: "globe", text: "Opens docs.rs", strong: ["elsewhere"]).pieces == [.init(text: "Opens docs.rs", bold: false)])
}

@Test func theMutedWordsAreTheHostsLabelThenThisMacForTheWhereLine() {
    #expect(CardFact(icon: "folder", text: "In a/b", muted: "its folder", place: true).note(hostName: "Mac mini") == "its folder \u{00B7} Mac mini")
    #expect(CardFact(icon: "folder", text: "In a/b", place: true).note(hostName: "Mac mini") == "Mac mini")
    #expect(CardFact(icon: "pencil", text: "Saves into a/b", muted: "its scratch").note(hostName: "Mac mini") == "its scratch")
    #expect(CardFact(icon: "pencil", text: "Saves into a/b").note(hostName: "Mac mini") == "")
}

@Test func everyIconWordHasASymbolAndAnUnknownOneIsADocument() {
    for icon in ["terminal", "down", "globe", "pencil", "trash", "stop", "folder", "eye", "doc"] {
        #expect(!CardFact(icon: icon, text: "x").symbol.isEmpty)
    }
    #expect(CardFact(icon: "down", text: "x").symbol == "arrow.down.circle")
    #expect(CardFact(icon: "stop", text: "x").symbol == "stop.circle")
    #expect(CardFact(icon: "never-heard-of-it", text: "x").symbol == "doc")
}

@Test func noFactsOrOnlyEmptyOnesMeansTodaysCard() throws {
    var data = EntryData()
    #expect(data.shownFacts == nil)
    data.facts = [CardFact(icon: "terminal", text: "")]
    #expect(data.shownFacts == nil)
    let json = #"{"intent":"Look around","facts":[{"icon":"terminal","text":"A shell command: ls, reads only","strong":["A shell command"]}],"detailsOpen":true,"check":"Safety check stopped it: x"}"#
    let decoded = try JSONDecoder().decode(EntryData.self, from: Data(json.utf8))
    #expect(decoded.intent == "Look around")
    #expect(decoded.shownFacts?.count == 1)
    #expect(decoded.detailsOpen == true)
    #expect(decoded.check == "Safety check stopped it: x")
    // An older host sends none of it.
    #expect(try JSONDecoder().decode(EntryData.self, from: Data(#"{"title":"Run ls"}"#.utf8)).shownFacts == nil)
}
