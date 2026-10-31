import Testing
@testable import KithUI

@Suite struct GoesToTests {
    private func agents(selected: String? = nil) -> [GoesToAgent] {
        [("a", "Ada", 5), ("b", "Bo", 9), ("c", "Cy", 1), ("d", "Di", 7)].map {
            GoesToAgent(key: $0.0, name: $0.1, lastAt: Int64($0.2), selected: $0.0 == selected)
        }
    }

    @Test func mostRecentWhenNothingSelected() {
        let r = GoesTo.route(agents(), query: "hello")
        #expect(r.target?.key == "b")
        #expect(r.alternatives.map(\.key) == ["d", "a"])
        #expect(r.text == "hello")
    }

    @Test func selectedChatBeatsRecency() {
        #expect(GoesTo.route(agents(selected: "c"), query: "hi").target?.key == "c")
    }

    @Test func mentionBeatsSelectedAndStripsName() {
        let r = GoesTo.route(agents(selected: "c"), query: "@ada fix it")
        #expect(r.target?.key == "a")
        #expect(r.how == .mention)
        #expect(r.alternatives.isEmpty)
        #expect(r.text == "fix it")
    }

    @Test func unknownMentionGoesNowhere() {
        let r = GoesTo.route(agents(), query: "@Adam hi")
        #expect(r.target == nil)
        #expect(!r.canSend)
    }

    @Test func tappedAgentWinsOverAuto() {
        #expect(GoesTo.route(agents(), query: "hi", override: "c").target?.key == "c")
    }

    @Test func noAgentsAndOneAgent() {
        #expect(GoesTo.route([], query: "hi").target == nil)
        let one = GoesTo.route([GoesToAgent(key: "a", name: "Ada", lastAt: 0)], query: "hi")
        #expect(one.alternatives.isEmpty)
        #expect(one.canSend)
    }
}
