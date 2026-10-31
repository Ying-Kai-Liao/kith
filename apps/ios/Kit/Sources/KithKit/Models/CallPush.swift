import Foundation

/// A VoIP push for a ringing call (the host's `kind: "call"` payload, sent only while no phone is connected).
/// It carries ids and a deadline in the clear; the agent's name and the reason are in `sealed`.
public struct CallPush: Equatable, Sendable {
    public var callId: String
    public var botId: String
    public var computerId: ComputerID
    public var ctx: String
    /// When the ring ends. The host's clock, capped to a minute and a half from receipt so skew can't ring for long.
    public var deadline: Date
    public var sealed: String?

    /// A call is one per computer: the id alone is only unique on its own computer.
    public var key: String { CallPush.key(computerId: computerId, callId: callId) }

    public static func key(computerId: ComputerID, callId: String) -> String { "\(computerId)/\(callId)" }

    /// Longest this app rings from the moment the push arrives.
    public static let maxRing: TimeInterval = 90

    /// nil when the payload is not a readable call push; the caller still has to report it to CallKit.
    public static func parse(_ payload: [AnyHashable: Any], now: Date = .now) -> CallPush? {
        guard payload["kind"] as? String == "call",
              let callId = payload["callId"] as? String, !callId.isEmpty,
              let botId = payload["botId"] as? String,
              let computerId = payload["computerId"] as? String, !computerId.isEmpty,
              let ctx = payload["ctx"] as? String,
              let ms = (payload["deadline"] as? NSNumber)?.doubleValue else { return nil }
        let deadline = min(Date(timeIntervalSince1970: ms / 1000), now.addingTimeInterval(maxRing))
        return CallPush(callId: callId, botId: botId, computerId: computerId, ctx: ctx, deadline: deadline,
                        sealed: (payload["sealed"] as? String).flatMap { $0.isEmpty ? nil : $0 })
    }

    /// What the sealed part holds.
    public struct Secret: Equatable, Sendable {
        public var name: String
        public var reason: String
        public var from: String?

        public static func decode(_ data: Data) -> Secret? {
            struct Wire: Decodable { var name: String?; var reason: String?; var from: String? }
            guard let wire = try? JSONDecoder().decode(Wire.self, from: data) else { return nil }
            return Secret(name: wire.name ?? "", reason: wire.reason ?? "", from: wire.from)
        }
    }

    /// Shown when the push has no readable name (an old pairing without a push key, or a damaged payload).
    public static let fallbackName = "Kith call"

    /// The caller name for CallKit.
    public static func callerName(_ secret: Secret?) -> String {
        let name = secret?.name.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return name.isEmpty ? fallbackName : name
    }
}

/// Why a call stopped ringing on this phone, in the system call UI's terms (CXCallEndedReason).
public enum CallEndReason: Equatable, Sendable {
    case answeredElsewhere, declinedElsewhere, unanswered, remoteEnded

    /// From the host's `call` event state. Anything but ringing ends the ring; unknown words read as ended.
    public static func forState(_ state: String) -> CallEndReason {
        switch state {
        case "accepted": .answeredElsewhere
        case "declined": .declinedElsewhere
        case "missed": .unanswered
        default: .remoteEnded
        }
    }
}

/// The calls the system call UI is showing, so one call is one ring: a repeated push, a repeated tap or an
/// event for a call it already owns changes nothing twice. Pure bookkeeping; CallKit itself lives in the app.
public struct CallLedger: Sendable {
    public enum Phase: Equatable, Sendable { case ringing, answering, answered }

    public struct Entry: Equatable, Sendable {
        public var uuid: UUID
        public var push: CallPush
        public var phase: Phase
    }

    public enum Add: Equatable, Sendable {
        case new(UUID)
        /// A second push for a call already shown, or one already past its deadline.
        case rejected
    }

    private var entries: [UUID: Entry] = [:]

    public init() {}

    public func entry(_ uuid: UUID) -> Entry? { entries[uuid] }
    public func entry(key: String) -> Entry? { entries.values.first { $0.push.key == key } }
    public func owns(key: String) -> Bool { entry(key: key) != nil }

    public mutating func add(_ push: CallPush, uuid: UUID = UUID(), now: Date = .now) -> Add {
        guard entry(key: push.key) == nil, push.deadline > now else { return .rejected }
        entries[uuid] = Entry(uuid: uuid, push: push, phase: .ringing)
        return .new(uuid)
    }

    /// Accept pressed. nil for a second press (or a call that is gone): accept only once.
    public mutating func beginAnswer(_ uuid: UUID) -> Entry? {
        guard var entry = entries[uuid], entry.phase == .ringing else { return nil }
        entry.phase = .answering
        entries[uuid] = entry
        return entry
    }

    public mutating func answered(_ uuid: UUID) {
        if entries[uuid]?.phase == .answering { entries[uuid]?.phase = .answered }
    }

    /// A `call` event. Returns the call to end when it says a ringing call is over; a call this phone already
    /// answered keeps going (the event is the answer's own echo).
    public mutating func settle(key: String, event: IncomingCallEvent) -> (UUID, CallEndReason)? {
        guard !event.ringing, let entry = entry(key: key), entry.phase == .ringing else { return nil }
        return (entry.uuid, .forState(event.state))
    }

    /// The ring is over for the calls nobody answered by `now`.
    public func expired(now: Date = .now) -> [UUID] {
        entries.values.filter { $0.phase == .ringing && $0.push.deadline <= now }.map(\.uuid)
    }

    @discardableResult
    public mutating func remove(_ uuid: UUID) -> Entry? { entries.removeValue(forKey: uuid) }
}
