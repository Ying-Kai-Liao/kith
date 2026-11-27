import KithKit
import Testing
@testable import KithUI

@Test func gistIsTheFirstSentence() {
    #expect(Gist.sentence(of: "Deploy is done. Two checks are still running.\n\nMore below.") == "Deploy is done.")
    #expect(Gist.sentence(of: "Is it live? Yes, since noon.") == "Is it live?")
}

@Test func gistStripsMarkdown() {
    #expect(Gist.sentence(of: "**Done.** The [report](https://x.test/a.b) is ready.") == "Done.")
    #expect(Gist.sentence(of: "## The `build` passed. Next steps follow.") == "The build passed.")
    #expect(Gist.sentence(of: "- First item here. And more.\n- Second") == "First item here.")
    #expect(Gist.sentence(of: "1. Run the tests. Then ship.") == "Run the tests.")
}

@Test func gistOfAReplyOpeningWithCodeOrATableIsNil() {
    #expect(Gist.sentence(of: "```swift\nlet a = 1.\n```\nThat is it.") == nil)
    #expect(Gist.sentence(of: "| a | b |\n|---|---|") == nil)
    #expect(Gist.sentence(of: "![chart](https://x.test/c.png)") == nil)
}

@Test func gistSkipsAbbreviationsUrlsAndDecimals() {
    #expect(Gist.sentence(of: "Use a tool, e.g. the linter, before you push. Then wait.") == "Use a tool, e.g. the linter, before you push.")
    #expect(Gist.sentence(of: "Ask Dr. Lee about it. She knows.") == "Ask Dr. Lee about it.")
    #expect(Gist.sentence(of: "See https://example.com/a.b/c for more. Thanks.") == "See https://example.com/a.b/c for more.")
    #expect(Gist.sentence(of: "Version 3.5 is out now. Update soon.") == "Version 3.5 is out now.")
    #expect(Gist.sentence(of: "Made by J. Smith in the U.S. last year. Yes.") == "Made by J. Smith in the U.S. last year.")
}

@Test func gistHandlesEmojiAndCJK() {
    #expect(Gist.sentence(of: "All set 🎉 Ready to go! Next one.") == "All set 🎉 Ready to go!")
    #expect(Gist.sentence(of: "部署完成。接下来检查日志。") == "部署完成。")
    #expect(Gist.sentence(of: "完成了！还有别的吗？") == "完成了！")
}

@Test func gistWithoutAnEndingMarkIsTheWholeFirstParagraph() {
    #expect(Gist.sentence(of: "No full stop here\nbut two lines\n\nSecond paragraph.") == "No full stop here but two lines")
    #expect(Gist.sentence(of: "   ") == nil)
}

@Test func shortRepliesDoNotFold() {
    #expect(Gist.folded("Done. All green.") == nil)
    #expect(Gist.folded("Just one line.") == nil)
}

@Test func longRepliesFoldToTheirGist() {
    let long = "The migration finished. " + String(repeating: "More detail about what happened. ", count: 12)
    #expect(Gist.folded(long) == "The migration finished.")
    #expect(Gist.folded("Fixed it.\n- a\n- b\n- c") == "Fixed it.")
    #expect(Gist.folded("Fixed it.\n```\ncode\n```") == "Fixed it.")
}

@Test func oneLongSentenceHasNothingToFoldTo() {
    #expect(Gist.folded(String(repeating: "word ", count: 80)) == nil)
}

private func entry(_ kind: String, _ text: String, status: String? = nil, _ edit: (inout EntryData) -> Void = { _ in }) -> Entry {
    var data = EntryData(text: text, status: status)
    edit(&data)
    return Entry(id: "a", seq: 1, botId: "b", rev: 1, kind: kind, turn: 1, data: data, createdAt: 0, updatedAt: 0)
}

private let longText = "It is done. " + String(repeating: "Here is what changed in detail. ", count: 12)

@Test func onlyFinishedLongAgentRepliesFold() {
    #expect(ReplyFold.reason(for: entry("agent", longText), gistMode: true) == .gist("It is done."))
    #expect(ReplyFold.reason(for: entry("agent", longText), gistMode: false) == nil)
    #expect(ReplyFold.reason(for: entry("agent", longText, status: "streaming"), gistMode: true) == nil)
    #expect(ReplyFold.reason(for: entry("agent", "Short."), gistMode: true) == nil)
    #expect(ReplyFold.reason(for: entry("agent", longText) { $0.unanswered = true }, gistMode: true) == nil)
}

@Test func yourOwnMessagesAndAsksNeverFold() {
    #expect(ReplyFold.reason(for: entry("user", longText), gistMode: true) == nil)
    #expect(ReplyFold.reason(for: entry("permission", longText), gistMode: true) == nil)
    #expect(ReplyFold.reason(for: entry("notice", longText), gistMode: true) == nil)
}

@Test func pinchBeyondTheThresholdSwitchesTheMode() {
    #expect(GistSetting.mode(forPinchScale: 0.6, current: false) == true)
    #expect(GistSetting.mode(forPinchScale: 1.5, current: true) == false)
    #expect(GistSetting.mode(forPinchScale: 0.9, current: false) == nil)
    #expect(GistSetting.mode(forPinchScale: 0.6, current: true) == nil)
    #expect(GistSetting.mode(forPinchScale: 1.5, current: false) == nil)
}
