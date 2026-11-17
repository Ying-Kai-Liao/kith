import Foundation
import Testing
@testable import KithKit

private let v = Vectors.shared

/// Counts dials; every dial fails, optionally after waiting on a gate so an attempt stays in flight.
private actor Dials {
    private(set) var count = 0
    private var gated = false
    private var gate: [CheckedContinuation<Void, Never>] = []

    func hold(_ on: Bool) {
        gated = on
        if !on { gate.forEach { $0.resume() }; gate = [] }
    }

    func dial() async {
        count += 1
        if gated { await withCheckedContinuation { gate.append($0) } }
    }
}

private func failingDialer(_ dials: Dials) -> Dialer {
    { _ in
        await dials.dial()
        throw HostError.unreachable
    }
}

private func relayOnly() -> Computer {
    var computer = v.computer
    computer.urls = []
    computer.cloud = URL(string: "https://cloud.example.dev")
    return computer
}

private func until(_ what: @escaping () async -> Bool) async -> Bool {
    for _ in 0..<400 {
        if await what() { return true }
        try? await Task.sleep(for: .milliseconds(10))
    }
    return false
}

private func backingOff(_ t: ChannelTransport) async -> Bool { await until { await t.inBackoff } }

private func transport(_ dials: Dials) -> ChannelTransport {
    ChannelTransport(computer: relayOnly(), identity: v.identity, pairingCode: nil, dial: failingDialer(dials),
                     watchesNetwork: false, retryDelay: { .seconds(60) })
}

@Test(.timeLimit(.minutes(1))) func kickInBackoffRetriesAtOnce() async throws {
    let dials = Dials()
    let t = transport(dials)
    await t.start()
    #expect(await backingOff(t))
    #expect(await dials.count == 1)
    let started = ContinuousClock.now
    await t.kick()
    #expect(await until { await dials.count == 2 })
    #expect(ContinuousClock.now - started < .seconds(5), "far under the 60 s backoff")
    await t.shutdown()
}

@Test(.timeLimit(.minutes(1))) func kickDuringAnAttemptStacksNothing() async throws {
    let dials = Dials()
    await dials.hold(true)
    let t = transport(dials)
    await t.start()
    #expect(await until { await dials.count == 1 })
    #expect(await !t.inBackoff)
    await t.kick()
    await t.kick()
    await dials.hold(false)
    // The attempt ends and the loop backs off; the early kicks were dropped, so no second dial.
    #expect(await backingOff(t))
    try await Task.sleep(for: .milliseconds(200))
    #expect(await dials.count == 1)
    await t.shutdown()
}

@Test(.timeLimit(.minutes(1))) func manyKicksMakeOneAttempt() async throws {
    let dials = Dials()
    let t = transport(dials)
    await t.start()
    #expect(await backingOff(t))
    await dials.hold(true)
    for _ in 0..<20 { await t.kick() }
    #expect(await until { await dials.count == 2 })
    try await Task.sleep(for: .milliseconds(200))
    #expect(await dials.count == 2, "one attempt, held in flight")
    await dials.hold(false)
    // The kicked attempt fails: back to waiting out the 60 s backoff, no hot loop.
    #expect(await backingOff(t))
    try await Task.sleep(for: .milliseconds(200))
    #expect(await dials.count == 2)
    await t.shutdown()
}

@Test(.timeLimit(.minutes(1))) func kickDoesNotResurrectAClosedTransport() async throws {
    let dials = Dials()
    let t = transport(dials)
    await t.start()
    #expect(await backingOff(t))
    await t.shutdown()
    await t.kick()
    try await Task.sleep(for: .milliseconds(100))
    #expect(await dials.count == 1)
}
