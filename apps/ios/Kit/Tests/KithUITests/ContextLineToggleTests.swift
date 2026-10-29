import Testing
@testable import KithUI

@Test func tappingAReplyOpensItsContextLineAndTappingAgainClosesIt() {
    #expect(ContextLineToggle.tapped("a", open: nil) == "a")
    #expect(ContextLineToggle.tapped("a", open: "a") == nil)
}

@Test func tappingAnotherReplyMovesTheOpenLine() {
    #expect(ContextLineToggle.tapped("b", open: "a") == "b")
}

@Test func doubleTapToggleTwiceEndsClosed() {
    let once = ContextLineToggle.tapped("a", open: nil)
    #expect(ContextLineToggle.tapped("a", open: once) == nil)
}
