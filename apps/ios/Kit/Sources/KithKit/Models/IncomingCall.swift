import Foundation

/// An agent calling the person, from the host's `call` event while it rings. It is on screen until any event settles
/// it (answered on a device, declined, missed) or its ring runs out here. A closed app hears it from a VoIP push instead (`CallPush`).
public struct IncomingCall: Identifiable, Equatable, Sendable {
    public var id: String
    public var botId: String
    /// The thread the agent called from, when it did.
    public var threadId: String?
    public var name: String
    public var reason: String
    /// When this app stops showing it: the host's remaining time from receipt, so clock skew doesn't matter.
    public var deadline: Date

    public init(id: String, botId: String, threadId: String? = nil, name: String, reason: String, deadline: Date) {
        self.id = id
        self.botId = botId
        self.threadId = threadId
        self.name = name
        self.reason = reason
        self.deadline = deadline
    }
}

/// What a `call` event says: the call, and whether it is still ringing.
public struct IncomingCallEvent: Equatable, Sendable {
    public var call: IncomingCall
    public var ringing: Bool
    /// The host's word for the call: "ringing", "accepted", "declined" or "missed".
    public var state: String

    public init(call: IncomingCall, ringing: Bool, state: String? = nil) {
        self.call = call
        self.ringing = ringing
        self.state = state ?? (ringing ? "ringing" : "")
    }

    /// The event's payload; nil for one this app can't read.
    public static func decode(_ data: Data, now: Date = .now) -> IncomingCallEvent? {
        struct Wire: Decodable {
            struct Call: Decodable {
                var id: String
                var botId: String
                var threadId: String?
                var name: String?
                var reason: String?
                var state: String?
                var remainingMs: Double?
            }
            var call: Call
        }
        guard let wire = try? JSONDecoder().decode(Wire.self, from: data) else { return nil }
        let c = wire.call
        let remaining = max(0, c.remainingMs ?? 45_000) / 1000
        return IncomingCallEvent(
            call: IncomingCall(id: c.id, botId: c.botId, threadId: c.threadId, name: c.name ?? "", reason: c.reason ?? "",
                               deadline: now.addingTimeInterval(remaining)),
            ringing: c.state == "ringing", state: c.state ?? "")
    }
}

public enum IncomingCalls {
    /// The calls on screen after an event: a ringing call is added (once), any other state takes it away.
    public static func apply(_ calls: [IncomingCall], _ event: IncomingCallEvent) -> [IncomingCall] {
        let rest = calls.filter { $0.id != event.call.id }
        guard event.ringing else { return rest }
        return rest + [calls.first { $0.id == event.call.id } ?? event.call]
    }

    /// Drops the calls whose ring is over.
    public static func expire(_ calls: [IncomingCall], now: Date = .now) -> [IncomingCall] {
        calls.filter { $0.deadline > now }
    }
}
