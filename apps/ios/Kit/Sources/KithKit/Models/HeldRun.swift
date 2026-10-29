import Foundation

/// An approved card's session can't resume because an old run still holds its writer claim (host `cards/resume.rs`).
/// `entry.data.held`; the host clears it when the run ends, is stopped, or the card expires.
public struct HeldRun: Codable, Hashable, Sendable {
    public var session: String
    /// terminal | oneshot | bot | app. Open string: an unknown kind gets the terminal wording.
    public var kind: String
    public var pid: Int
    /// When the claim was taken (RFC 3339).
    public var since: String

    public init(session: String, kind: String, pid: Int, since: String) {
        self.session = session
        self.kind = kind
        self.pid = pid
        self.since = since
    }

    public var sinceDate: Date? { HeldRun.parse(since) }

    static func parse(_ text: String) -> Date? {
        let plain = ISO8601DateFormatter()
        let fractional = ISO8601DateFormatter()
        fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        return fractional.date(from: text) ?? plain.date(from: text)
    }

    /// `9:40`, `yesterday 21:10` past midnight, a weekday inside a week, a date past it.
    public func sinceText(now: Date = .now, calendar: Calendar = .current) -> String {
        guard let date = sinceDate else { return "" }
        return HeldRun.clock(date, now: now, calendar: calendar)
    }

    static func clock(_ date: Date, now: Date, calendar: Calendar) -> String {
        let f = DateFormatter()
        f.calendar = calendar
        f.timeZone = calendar.timeZone
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "H:mm"
        let time = f.string(from: date)
        if calendar.isDate(date, inSameDayAs: now) { return time }
        if let yesterday = calendar.date(byAdding: .day, value: -1, to: now), calendar.isDate(date, inSameDayAs: yesterday) { return "yesterday \(time)" }
        if now.timeIntervalSince(date) < 7 * 86_400 { return "\(calendar.shortWeekdaySymbols[calendar.component(.weekday, from: date) - 1]) \(time)" }
        f.dateFormat = "MMM d"
        return f.string(from: date)
    }
}

/// An old run the owner stopped from a card: the transcript's divider says so.
public struct StoppedRun: Hashable, Sendable {
    public var kind: String
    public var at: Date

    public init(kind: String, at: Date) {
        self.kind = kind
        self.at = at
    }
}

/// What `stopHolder` answers: `stopped` is who was ended; nil when nothing live held the session any more (already gone,
/// or a reused pid left alone). `ownTurn` is set when the holder is the host's own turn, which Stop never ends.
public struct StopHolderResult: Codable, Hashable, Sendable {
    public struct Who: Codable, Hashable, Sendable {
        public var kind: String
        public var pid: Int
        public var since: String
    }
    public var stopped: Who?
    public var ownTurn: Who?
}

/// The card's held row, per state: copy by kind, who sees Stop, and the confirm text (designer's spec `held-run`).
public struct HeldRunModel: Equatable, Sendable {
    public enum Phase: Equatable, Sendable {
        case idle, stopping
        /// Stop failed with this reason; `retry` is false when trying again can't help.
        case failed(String, retry: Bool)
    }

    public let held: HeldRun
    public let mac: String
    /// Only the computer's owner stops it; members and bot guests read who can.
    public let owner: Bool
    /// The owner's name for the sentence a member reads.
    public let ownerName: String
    public var phase: Phase
    public var now: Date

    public init(held: HeldRun, mac: String, owner: Bool, ownerName: String? = nil, phase: Phase = .idle, now: Date = .now) {
        self.held = held
        self.mac = mac
        self.owner = owner
        self.ownerName = ownerName ?? "the owner"
        self.phase = phase
        self.now = now
    }

    /// The wording of a kind: "terminal run", "background run", "chat run", "app turn".
    private var noun: String { HeldRunModel.noun(held.kind) }

    static func noun(_ kind: String) -> String {
        switch kind {
        case "oneshot": "background run"
        case "bot": "chat run"
        case "app": "app turn"
        default: "terminal run"
        }
    }

    /// An SF Symbol name.
    public var icon: String { HeldRunModel.symbol(held.kind) }

    public static func symbol(_ kind: String) -> String {
        switch kind {
        case "oneshot": "gearshape.2"
        case "bot": "bubble.left.and.bubble.right"
        case "app": "macwindow"
        default: "terminal"
        }
    }

    public var title: String { "An old \(noun) still has this session" }

    private var since: String { held.sinceText(now: now) }

    /// Stop shows for the owner, until a failure says it can't help.
    public var canStop: Bool {
        guard owner else { return false }
        if case .failed(_, retry: false) = phase { return false }
        return true
    }

    public var buttonTitle: String {
        if case .failed(_, retry: true) = phase { return "Try again" }
        return "Stop"
    }

    public var sentence: String {
        switch phase {
        case .stopping: return "Stopping it…"
        case let .failed(reason, _): return "Couldn't stop it. \(reason)"
        case .idle:
            let base = "Since \(since). It goes on when that ends"
            return owner ? base + "." : base + ", or when \(ownerName) stops it."
        }
    }

    public var isFailed: Bool {
        if case .failed = phase { return true }
        return false
    }

    public var confirmTitle: String { "Stop the old run?" }

    public func confirmMessage(agent: String) -> String {
        let what = held.kind == "app" ? "An app turn" : held.kind == "bot" ? "A chat run" : held.kind == "oneshot" ? "A background run" : "A terminal session"
        return "\(what) on \(mac) has had this session since \(since). Kith ends it (pid \(held.pid)) so \(agent) can go on with what you allowed. Anything it's in the middle of stops too."
    }

    public var accessibilityLabel: String { "\(title), since \(since). It goes on when that ends." }
    public var buttonHint: String { "Ends the \(held.kind == "terminal" ? "terminal session" : noun) on \(mac) so the agent can go on." }

    /// The transcript divider after a Stop: "Stopped the old terminal run · 10:02".
    public static func stoppedDivider(kind: String, at: Date, calendar: Calendar = .current) -> String {
        "Stopped the old \(noun(kind)) · \(clockText(at, calendar: calendar))"
    }

    static func clockText(_ date: Date, calendar: Calendar) -> String { HeldRun.clock(date, now: date, calendar: calendar) }

    /// How a failed `stopHolder` reads. A refusal means this account isn't the owner (the button should not have shown).
    public static func failure(for error: Error, mac: String) -> Phase {
        if case let HostError.http(code, _) = error, code == 403 { return .failed("Only the owner can stop it.", retry: false) }
        return .failed("Try again, or close it on \(mac).", retry: true)
    }

    public static let ownTurnPhase = Phase.failed("That's Kith's own turn. It ends by itself.", retry: false)
}

public extension HostClient {
    /// Ends the old run that holds a card's session so the approved resume can go on. Owner only: members and bot
    /// guests are refused by the host. Succeeds quietly when nobody live holds it any more.
    func stopHolder(session: String) async throws -> StopHolderResult {
        struct Body: Encodable { var session: String }
        return try await call("stopHolder", Body(session: session), timeout: 20)
    }
}

public extension Entry {
    /// An answered permission card an old run still holds up. It is not a card that waits on the reader.
    var isHeldPermission: Bool { kind == "permission" && data.status == "answered" && data.held != nil }
}
