import Testing
@testable import KithKit
@testable import KithUI

@Suite struct ComponentsTests {
    @Test func skyToneIsStablePerIdAndYouAreDawn() {
        for id in ["kai", "mia", "ren", "e43d5d5"] {
            #expect(SkyTone.tone(for: id) == SkyTone.tone(for: id))
            #expect(SkyTone.crop(for: id).x == SkyTone.crop(for: id).x)
        }
        #expect(SkyTone.tone(for: "anything", isYou: true) == .dawn)
        // FNV-1a is fixed, so a given id keeps its tone across launches and releases.
        #expect(SkyTone.hash("kai") == SkyTone.hash("kai"))
        #expect(SkyTone.hash("") == 0xcbf29ce484222325)
        #expect(Set(["a", "b", "c", "d", "e", "f", "g", "h"].map { SkyTone.tone(for: $0) }).count > 1)
    }

    @Test func countBadgeText() {
        #expect(CountBadge.text(for: 0) == nil)
        #expect(CountBadge.text(for: -2) == nil)
        #expect(CountBadge.text(for: 1) == "1")
        #expect(CountBadge.text(for: 99) == "99")
        #expect(CountBadge.text(for: 100) == "99+")
    }

    @Test func dockCallButtonRule() {
        #expect(ChatDockLayout.callVisible(showsCall: true, text: "", hasAttachment: false))
        #expect(!ChatDockLayout.callVisible(showsCall: true, text: "hi", hasAttachment: false))
        #expect(ChatDockLayout.callVisible(showsCall: true, text: "  \n", hasAttachment: false))
        #expect(!ChatDockLayout.callVisible(showsCall: true, text: "", hasAttachment: true))
        #expect(!ChatDockLayout.callVisible(showsCall: false, text: "", hasAttachment: false))
        #expect(ChatDockLayout.showsSend(text: "hi", hasAttachment: false))
        #expect(!ChatDockLayout.showsSend(text: "", hasAttachment: false))
    }

    @Test func handoffHidesEmptyFields() {
        #expect(HandoffCardModel().isEmpty)
        #expect(HandoffCardModel(done: "  ", next: "").isEmpty)
        #expect(!HandoffCardModel(next: "Run doctor").isEmpty)
        #expect(HandoffCardModel(done: " ok ").doneText == "ok")
    }

    @Test func agentTintFollowsThePaletteRule() {
        #expect(AgentTint(stored: "kith:coral") != nil)
        #expect(AgentTint(stored: "blue") == nil)
        #expect(AgentTint(stored: "kith:nope") == nil)
        #expect(AgentTint(stored: "kith:coral:plain")?.b == AgentTint(stored: "kith:coral:plain")?.a)
        #expect(AgentTint(stored: "kith:coral")?.b != AgentTint(stored: "kith:coral")?.a)
        #expect(AgentTint.resolve("blue") == .neutral)
    }

    @Test func decisionOptionsLayout() {
        #expect(DecisionCardModel.permission(request: "x").fitsInRow)
        let long = DecisionCardModel(question: "q", options: [.init(id: "a", title: "A rather long answer label"), .init(id: "b", title: "B")])
        #expect(!long.fitsInRow)
        let four = DecisionCardModel(question: "q", options: (0..<4).map { .init(id: "\($0)", title: "x") })
        #expect(!four.fitsInRow)
    }

    @Test func statusChipWords() {
        #expect(StatusChip.Kind.urgent.word == "/urgent")
        #expect(StatusChip.Kind.busy(computer: "Mac mini", queued: 1).word == "Mac mini busy, 1 queued")
        #expect(StatusChip.Kind.busy(computer: "Mac mini", queued: 0).word == "Mac mini busy")
    }
}
