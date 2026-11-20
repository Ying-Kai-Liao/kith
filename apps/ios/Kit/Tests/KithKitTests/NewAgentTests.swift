import Foundation
import Testing
@testable import KithKit

private func bot(_ json: String) throws -> Bot {
    try JSONDecoder().decode(Bot.self, from: Data(json.utf8))
}

@Test func newAgentNamesFollowTheHostsRules() {
    #expect(NewAgent.nameError("trip-planner") == nil)
    #expect(NewAgent.nameError("") != nil)
    #expect(NewAgent.nameError("a") != nil)
    #expect(NewAgent.nameError("9lives") != nil)
    #expect(NewAgent.nameError("Trip") != nil)
    #expect(NewAgent.nameError("kith") != nil)
    #expect(NewAgent.suggestedName("  My Trip Bot!") == "my-trip-bot")
}

@Test func newAgentDescriptionIsOneLineWithoutAColonSpace() {
    #expect(NewAgent.descriptionError("Plans trips") == nil)
    #expect(NewAgent.descriptionError("") != nil)
    #expect(NewAgent.descriptionError("-x") != nil)
    #expect(NewAgent.descriptionError("a: b") != nil)
}

@Test func aFlaggedRowPrefillsTheForm() throws {
    let flagged = try bot(#"{"id":"n","name":"Trip","description":"\nPlans trips: books them.\nNever pays.","backend":"codex","model":"gpt-5","needsKithAgent":true}"#)
    let form = NewAgent.Form(like: flagged)
    #expect(form == NewAgent.Form(name: "trip", description: "Plans trips - books them.", engine: "codex", model: "gpt-5"))
    #expect(NewAgent.descriptionError(form.description) == nil)
    #expect(form.params == ["name": "trip", "description": "Plans trips - books them.", "harness": "codex", "model": "gpt-5"])
    // An engine kith can't run stays at its default, and takes the model with it.
    let odd = NewAgent.Form(like: try bot(#"{"id":"o","name":"Odd","backend":"gemini","model":"x"}"#))
    #expect(odd.engine == "" && odd.model == "")
}
