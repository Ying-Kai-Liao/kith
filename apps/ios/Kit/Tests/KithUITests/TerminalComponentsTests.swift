import Testing
@testable import KithKit
@testable import KithUI

/// Ports the writer cases of apps/desktop/.../kith/logic.test.ts: the iOS wording must match the desktop's.
@Suite struct TerminalComponentsTests {
    @Test func bannerMapsEachStateToCopyAndButton() {
        let mac = "Mac mini"
        let idle = WriterBannerModel.of(phase: .idle, mac: mac)
        #expect(idle == .init(icon: .terminal, title: "Open in terminal", sentence: "On Mac mini. Reply there, or take over.", button: .takeOver))
        #expect(WriterBannerModel.of(phase: .working, mac: mac).button == nil)
        #expect(WriterBannerModel.of(phase: .working, mac: mac).icon == .dot)
        #expect(WriterBannerModel.of(phase: .working, mac: mac).sentence == "On Mac mini. You can take over when it stops.")
        let taking = WriterBannerModel.of(phase: .taking, mac: mac)
        #expect(taking.title == "Taking over…")
        #expect(taking.icon == .spinner)
        #expect(taking.sentence == "Closing the terminal session on Mac mini.")
        #expect(taking.button == nil)
        let failed = WriterBannerModel.of(phase: .failed, mac: mac)
        #expect(failed.title == "Couldn't take over")
        #expect(failed.button == .tryAgain)
        let gone = WriterBannerModel.of(phase: .unreachable, mac: mac)
        #expect(gone.title == "Mac mini unreachable")
        #expect(gone.icon == .warning)
        #expect(gone.button == nil)
    }

    @Test func takeOverIsOnlyForTheTerminalsOwner() {
        let other = WriterBannerModel.of(phase: .idle, mac: "Mac mini", mine: false, person: "Mia")
        #expect(other.title == "Open in Mia's terminal")
        #expect(other.sentence == "On Mac mini.")
        #expect(other.button == nil)
        #expect(WriterBannerModel.of(phase: .idle, mac: "x", mine: false).title == "Open in their terminal")
        #expect(WriterBannerModel.of(phase: .failed, mac: "x", mine: false).button == nil)
    }

    @Test func resumeCommandNamesAgentAndSession() {
        #expect(WriterBannerModel.resumeCommand(agent: "coder", resumeId: "abc123") == "kith coder --resume abc123")
    }

    @Test func writerChipWords() {
        #expect(StatusChip.Kind.writer(working: false, mac: nil).word == "In terminal")
        #expect(StatusChip.Kind.writer(working: true, mac: "Mac mini").word == "Working in terminal · Mac mini")
    }

    @Test func dividerAndOriginWords() {
        #expect(WriterDivider.text(.startedInTerminal(mac: "Mac mini"), time: "9:40") == "Started in the terminal on Mac mini · 9:40")
        #expect(WriterDivider.text(.openedInTerminal(mac: "Mac mini"), time: "10:31") == "Opened in terminal on Mac mini · 10:31")
        #expect(WriterDivider.text(.tookOver, time: "10:02") == "You took over from the terminal · 10:02")
        #expect(WriterDivider.text(.terminalClosed, time: "10:48") == "Terminal closed · 10:48")
        #expect(OriginTag.Origin.allCases.map(\.word) == ["Terminal", "Discord", "Telegram", "Slack", "Bot"])
    }
}
