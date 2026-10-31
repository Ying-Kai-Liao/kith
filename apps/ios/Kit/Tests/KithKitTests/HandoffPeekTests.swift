import Foundation
import Testing
@testable import KithKit

private func peek(_ over: String = "") throws -> HandoffPeek {
    let json = """
    {"id":"bg-20261031120000-abc123","state":"working","brief":"Write the plan",
     "from":{"agent":"architect","person":"mia"},"to":"echo","crossed":false,
     "thread":{"botId":"echo","threadId":"t1","sessionId":"s"},
     "live":{"kind":"tool","text":"Edited plan.md"},
     "limits":{"costMicroUsd":250000,"capMicroUsd":5000000,"chainCostMicroUsd":700000,"hop":1,"hopMax":8,"crossHops":0,"crossMax":2,"elapsedMs":90000},
     "startedMs":1000,"answeredMs":null,"cappedMs":null\(over)}
    """
    return try JSONDecoder().decode(HandoffPeek.self, from: Data(json.replacingOccurrences(of: "\n", with: "").utf8))
}

@Test func aPeekReadsAsTheHostSentIt() throws {
    let p = try peek()
    #expect(p.state == .working && p.title == "architect → echo")
    #expect(p.thread == .init(botId: "echo", threadId: "t1") && p.live?.text == "Edited plan.md")
}

@Test func anUnknownStateReadsAsUnknownAndNullsStayNil() throws {
    let data = Data(#"{"id":"x","state":"weird","thread":null,"live":null,"limits":{}}"#.utf8)
    let p = try JSONDecoder().decode(HandoffPeek.self, from: data)
    #expect(p.state == .unknown && p.thread == nil && p.live == nil && p.answeredMs == nil)
}

@Test func theAskIdComesFromAKithAskRowAndPlainAsksHaveNone() {
    let id = "bg-20261031120000-abc123"
    #expect(HandoffPeek.askId(command: "kith ask --bg echo \"x\"", output: "started echo in the background as \(id); its answer: kith ask --wait \(id)") == id)
    #expect(HandoffPeek.askId(command: "kith ask --wait \(id)", output: nil) == id)
    #expect(HandoffPeek.askId(command: "kith ask echo \"x\"", output: #"{"waiting":true,"id":"ask-20261031120000-0a1b2c"}"#) == "ask-20261031120000-0a1b2c")
    #expect(HandoffPeek.askId(command: "kith ask echo \"x\"", output: "the answer") == nil)
    #expect(HandoffPeek.askId(command: "ls", output: id) == nil)
    #expect(HandoffPeek.askId(command: nil, output: id) == nil)
}

@Test func theLimitsLineFlagsTheOneClosestToItsLimit() throws {
    let parts = try peek().limitParts
    #expect(parts.first?.label == "$0.70 / $5")
    #expect(parts.filter(\.watch).map(\.kind) == [.budget])
    let hops = try peek().limitParts.map(\.label)
    #expect(hops.last == "1 min")
    var p = try peek()
    p.limits.hop = 7
    #expect(p.limitParts.filter(\.watch).map(\.kind) == [.hops])
}

@Test func joinStopAndTheBoxSayWhyTheyAreOff() throws {
    var p = try peek()
    #expect(p.joinBlocked == nil && p.stopBlocked == nil && p.sendBlocked == nil)
    p.thread = nil
    p.crossed = true
    #expect(p.joinBlocked == "It runs on another Mac" && p.stopBlocked == "It runs on another Mac")
    var done = try peek()
    done.state = .done
    #expect(done.joinBlocked == nil, "Join still opens a finished thread")
    #expect(done.stopBlocked == "It is not running" && done.sendBlocked == "It is not running")
}
