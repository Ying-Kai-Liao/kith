import Foundation
import Testing
@testable import KithKit

@Suite("Call pushes")
struct CallPushTests {
    private let now = Date(timeIntervalSince1970: 1_000)

    private func payload(deadline: Double? = 1_030_000, sealed: String? = "abc") -> [AnyHashable: Any] {
        var p: [AnyHashable: Any] = ["kind": "call", "callId": "c1", "botId": "b1", "computerId": "mac", "ctx": "local"]
        if let deadline { p["deadline"] = NSNumber(value: deadline) }
        if let sealed { p["sealed"] = sealed }
        return p
    }

    private func event(_ state: String, id: String = "c1") -> IncomingCallEvent {
        let json = #"{"call":{"id":"\#(id)","botId":"b1","state":"\#(state)","remainingMs":1000}}"#
        return IncomingCallEvent.decode(Data(json.utf8), now: now)!
    }

    @Test func aCallPushIsReadFromItsPayload() throws {
        let push = try #require(CallPush.parse(payload(), now: now))
        #expect(push.key == "mac/c1")
        #expect(push.deadline == Date(timeIntervalSince1970: 1_030))
        #expect(push.sealed == "abc")
        #expect(CallPush.parse(payload(sealed: nil), now: now)?.sealed == nil)
        #expect(CallPush.parse(payload(sealed: ""), now: now)?.sealed == nil)
    }

    @Test func aDamagedPayloadIsNotACallPush() {
        #expect(CallPush.parse(payload(deadline: nil), now: now) == nil)
        #expect(CallPush.parse([:], now: now) == nil)
        var other = payload()
        other["kind"] = "done"
        #expect(CallPush.parse(other, now: now) == nil)
        var noId = payload()
        noId["callId"] = ""
        #expect(CallPush.parse(noId, now: now) == nil)
    }

    @Test func aFarDeadlineIsCappedSoSkewCannotRingForLong() throws {
        let push = try #require(CallPush.parse(payload(deadline: 9_999_999_000), now: now))
        #expect(push.deadline == now.addingTimeInterval(CallPush.maxRing))
    }

    @Test func theSecretGivesTheCallerName() {
        let secret = CallPush.Secret.decode(Data(#"{"name":"House","reason":"the bill","from":"m1","faces":[]}"#.utf8))
        #expect(secret == CallPush.Secret(name: "House", reason: "the bill", from: "m1"))
        #expect(CallPush.callerName(secret) == "House")
        #expect(CallPush.callerName(nil) == "Kith call")
        #expect(CallPush.callerName(CallPush.Secret(name: "  ", reason: "x", from: nil)) == "Kith call")
        #expect(CallPush.Secret.decode(Data("not json".utf8)) == nil)
    }

    @Test func aCallEventStateMapsToWhyTheRingEnded() {
        #expect(CallEndReason.forState("accepted") == .answeredElsewhere)
        #expect(CallEndReason.forState("declined") == .declinedElsewhere)
        #expect(CallEndReason.forState("missed") == .unanswered)
        #expect(CallEndReason.forState("") == .remoteEnded)
        #expect(CallEndReason.forState("ended") == .remoteEnded)
    }

    @Test func oneCallRingsOnceAndAcceptsOnce() throws {
        var ledger = CallLedger()
        let push = try #require(CallPush.parse(payload(), now: now))
        guard case let .new(uuid) = ledger.add(push, now: now) else { Issue.record("not added"); return }
        #expect(ledger.add(push, now: now) == .rejected, "a second push for the same call")
        #expect(ledger.owns(key: "mac/c1"))
        #expect(!ledger.owns(key: "other/c1"), "call ids are per computer")
        #expect(ledger.beginAnswer(uuid)?.phase == .answering)
        #expect(ledger.beginAnswer(uuid) == nil, "a double tap")
    }

    @Test func aPushPastItsDeadlineIsNotRung() throws {
        var ledger = CallLedger()
        let push = try #require(CallPush.parse(payload(deadline: 999_000), now: now))
        #expect(ledger.add(push, now: now) == .rejected)
    }

    @Test func aSettledEventEndsARingingCallButNotOneWeAnswered() throws {
        var ledger = CallLedger()
        let push = try #require(CallPush.parse(payload(), now: now))
        guard case let .new(uuid) = ledger.add(push, now: now) else { Issue.record("not added"); return }
        #expect(ledger.settle(key: push.key, event: event("ringing")) == nil)
        #expect(ledger.settle(key: "mac/zzz", event: event("missed", id: "zzz")) == nil)
        let ended = ledger.settle(key: push.key, event: event("declined"))
        #expect(ended?.0 == uuid && ended?.1 == .declinedElsewhere)

        _ = ledger.beginAnswer(uuid)
        #expect(ledger.settle(key: push.key, event: event("accepted")) == nil, "our own accept comes back as an event")
    }

    @Test func aHelloEndsOnlyRingingCallsTheHostDidNotRepeat() throws {
        var ledger = CallLedger()
        let push = try #require(CallPush.parse(payload(), now: now))
        guard case let .new(uuid) = ledger.add(push, now: now) else { Issue.record("not added"); return }
        #expect(ledger.staleAfterHello(computerId: "mac", ringing: ["c1"]).isEmpty, "still ringing on the host, also after a reconnect")
        #expect(ledger.staleAfterHello(computerId: "other", ringing: []).isEmpty, "a hello from another computer")
        #expect(ledger.staleAfterHello(computerId: "mac", ringing: []) == [uuid])
        _ = ledger.beginAnswer(uuid)
        #expect(ledger.staleAfterHello(computerId: "mac", ringing: []).isEmpty, "the voice call owns an answered call")
        #expect(CallLedger().staleAfterHello(computerId: "mac", ringing: []).isEmpty, "nothing held")
    }

    @Test func aRingNobodyAnsweredExpiresAtItsDeadline() throws {
        var ledger = CallLedger()
        let push = try #require(CallPush.parse(payload(), now: now))
        guard case let .new(uuid) = ledger.add(push, now: now) else { Issue.record("not added"); return }
        #expect(ledger.expired(now: now.addingTimeInterval(29)).isEmpty)
        #expect(ledger.expired(now: now.addingTimeInterval(31)) == [uuid])
        _ = ledger.beginAnswer(uuid)
        #expect(ledger.expired(now: now.addingTimeInterval(31)).isEmpty, "an answered call is not on a timer")
        ledger.remove(uuid)
        #expect(!ledger.owns(key: push.key))
    }
}
