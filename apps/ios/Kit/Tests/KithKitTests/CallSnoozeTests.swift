import Foundation
import Testing
@testable import KithKit

@Suite("Call snooze")
struct CallSnoozeTests {
    private let snooze = CallSnooze(ctx: "acct", computerId: "mac1", botId: "b1", name: "House")

    @Test func ringsAgainInTenMinutes() {
        #expect(CallSnooze.delay == 600)
        #expect(snooze.title == "House is calling")
    }

    @Test func oneIdentifierPerAgent() {
        let again = CallSnooze(ctx: "acct", computerId: "mac1", botId: "b1", name: "House")
        let other = CallSnooze(ctx: "acct", computerId: "mac1", botId: "b2", name: "Coder")
        #expect(again.identifier == snooze.identifier)
        #expect(other.identifier != snooze.identifier)
    }

    @Test func payloadOpensTheCall() {
        let info = snooze.userInfo
        #expect(info["botId"] == "b1" && info["computerId"] == "mac1" && info["ctx"] == "acct")
        #expect(CallSnooze.isCallBack(info))
        #expect(!CallSnooze.isCallBack(["botId": "b1"]))
    }
}
