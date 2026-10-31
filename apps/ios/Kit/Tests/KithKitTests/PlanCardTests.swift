import Foundation
import Testing
@testable import KithKit

private func step(_ text: String, _ state: String) -> RichCard.JSON {
    .object(["text": .string(text), "state": .string(state)])
}

private func card(done: Int64, total: Int64, ended: Bool = false, _ steps: [RichCard.JSON]) -> RichCard {
    RichCard(type: "plan", v: 1, ref: ["entry": .string("e1")], snapshot: [
        "title": .string("Plan"), "done": .int(done), "total": .int(total), "ended": .bool(ended), "steps": .array(steps),
    ])
}

@Test func thePlanCardShowsTheCountTheBarAndEachStep() {
    let v = PlanCardModel(card: card(done: 2, total: 4, [step("Read", "done"), step("Edit", "done"), step("Test", "now"), step("Ship", "next")]))
    #expect(v.count == "2 of 4" && v.fraction == 0.5 && v.ended == false)
    #expect(v.steps.map(\.state) == [.done, .done, .now, .next])
}

@Test func aPlanThatEndedUnfinishedKeepsAStoppedStepAndAnUnknownStateReadsAsNext() {
    let v = PlanCardModel(card: card(done: 1, total: 3, ended: true, [step("a", "done"), step("b", "stopped"), step("c", "sideways")]))
    #expect(v.steps.map(\.state) == [.done, .stopped, .next] && v.ended)
}

@Test func aPlanWithNoStepsOrOddNumbersDoesNotBreak() {
    #expect(PlanCardModel(card: RichCard(type: "plan", v: 1)).count == "0 of 0")
    #expect(PlanCardModel(card: card(done: 9, total: 1, [step("x", "done"), .null, step("", "done")])).count == "1 of 1")
}

@Test func thisAppDrawsThePlanCard() {
    #expect(RichCard(type: "plan", v: 1).isDrawable())
    #expect(RichCard(type: "plan", v: 2).isDrawable() == false)
}
