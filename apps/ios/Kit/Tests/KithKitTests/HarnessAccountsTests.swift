import Foundation
import Testing
@testable import KithKit

private let sample = """
{"policy":"spread","crossHarness":true,
 "accounts":[
  {"id":"claude","harness":"claude","account":"default","kind":"login","inSet":true,"order":1,"status":"out","ready":true,
   "why":null,"fix":null,"outUntil":4000000,"reason":"rate_limit",
   "usage":{"readAt":1000,"windows":[{"name":"5h","percent":96.5,"resetsAt":4000000},{"name":"week","percent":10}]},
   "lastStarted":null,"pins":[{"agent":"house","only":true}],"loggedIn":true},
  {"id":"codex:deep","harness":"codex","account":"deep","inSet":true,"order":0,"status":"signed_out","ready":false,
   "why":"not logged in","fix":"codex login","usage":null,"pins":[],"loggedIn":false},
  {"id":"claude:old","harness":"claude","account":"old","inSet":false,"status":"someday","ready":false},
  {"nope":1}, 7],
 "agents":{"house":{"harness":"claude","pin":{"id":"claude","only":true},"own":null,"order":["claude"],"main":"claude",
   "candidates":[],"allOut":true,"crossHarness":false,
   "skipped":[{"id":"claude","status":"out","why":null,"outUntil":4000000,"reason":"rate_limit"}]},
  "bob":{"harness":"codex","pin":null,"own":["codex:deep"],"order":["codex:deep"],"candidates":["codex:deep"],
   "allOut":false,"crossHarness":true,"skipped":[]},
  "broken":5}}
"""

@Test func harnessAccountsDecode() throws {
    let a = try JSONDecoder().decode(HarnessAccounts.self, from: Data(sample.utf8))
    #expect(a.policy == "spread" && a.crossHarness)
    #expect(a.accounts.map(\.id) == ["claude", "codex:deep", "claude:old"], "odd entries drop out alone")
    #expect(a.set.map(\.id) == ["codex:deep", "claude"], "set order follows `order`")
    #expect(a.outsideSet.map(\.id) == ["claude:old"])
    let claude = a.account("claude")
    #expect(claude.status == .out && claude.outUntil == Date(timeIntervalSince1970: 4000))
    #expect(claude.windows.map(\.name) == ["5h", "week"] && claude.windows[0].percent == 96.5)
    #expect(claude.windows[0].resetsAt == Date(timeIntervalSince1970: 4000) && claude.windows[1].resetsAt == nil)
    #expect(claude.pins == [.init(agent: "house", only: true)])
    #expect(claude.state(now: Date(timeIntervalSince1970: 0)).contains("rate limit"))
    let deep = a.account("codex:deep")
    #expect(deep.status == .signedOut && deep.fix == "codex login" && deep.windows.isEmpty && !deep.loggedIn)
    #expect(a.account("claude:old").status == .other("someday"), "an unknown status doesn't fail the read")
    #expect(a.agentNames == ["bob", "house"], "an agent of an odd shape is left out")
    let house = try #require(a.agents["house"])
    #expect(house.pin == .init(id: "claude", only: true) && house.own == nil && house.allOut)
    #expect(house.skipped.first?.outUntil == Date(timeIntervalSince1970: 4000))
    let bob = try #require(a.agents["bob"])
    #expect(bob.pin == nil && bob.own == ["codex:deep"] && bob.crossHarness)
}

@Test func emptyHarnessAccounts() throws {
    let a = try JSONDecoder().decode(HarnessAccounts.self, from: Data("{}".utf8))
    #expect(a.accounts.isEmpty && a.agents.isEmpty && a.policy == "fill" && a.set.isEmpty)
}
