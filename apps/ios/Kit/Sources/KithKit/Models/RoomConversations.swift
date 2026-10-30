import Foundation

// What the app knows of a group room's conversations: one person message to a group gives the room some turns, and the
// host reports where that stands (`conversation` event, the `conversation` of a `history` answer). The words here are the
// designer's: "turns", and "Bot" for one a person brought in (docs/design/DESIGN.md, Words).

public typealias RoomConversation = HostContract.RoomConversation

/// How many turns a message to a group gives the room.
public enum Turns {
    public static let minimum = 1
    public static let standard = 10
    public static let maximum = 100
    /// The quick picks under the stepper.
    public static let picks = [1, 5, 10, 25, 100]

    public static func clamp(_ n: Int) -> Int { min(maximum, max(minimum, n)) }

    /// "1 turn", "10 turns".
    public static func words(_ n: Int) -> String { n == 1 ? "1 turn" : "\(n) turns" }
}

/// Why a reply came late; the host's word. A reason this version doesn't know shows the Late tag with no reason line.
public enum LateWhy: String, Sendable {
    case stopped, newer, ended, noTurn

    /// The muted line under a late reply: what lateness means (it started nothing), and why it is late.
    public var line: String {
        switch self {
        case .stopped: "Came after you stopped, so it starts nothing."
        case .newer: "Came after your newer message, so it starts nothing."
        case .ended: "Came after the turns ran out, so it starts nothing."
        case .noTurn: "Nobody was waiting for this, so it starts nothing."
        }
    }
}

extension EntryData {
    /// A late reply: Late tag, outlined bubble. An unprompted bot post is never late, whatever else it says.
    public var isLate: Bool { late == true && unprompted != true }

    public var lateReason: LateWhy? { lateWhy.flatMap(LateWhy.init(rawValue:)) }

    /// The bot behind a post in a room: a user entry the host wrote for an outside bot (`sender`). Nil for the owner's own.
    public var roomBot: EntrySender? {
        guard let sender, sender.botId?.isEmpty == false else { return nil }
        return sender
    }
}

extension Entry {
    /// An outside bot's post in a room: the host stores it as a user entry with a `sender`.
    public var isRoomBotPost: Bool { kind == "user" && data.roomBot != nil }

    /// The host's quiet lines about a bot in a room.
    public var isRoomNotice: Bool {
        kind == "notice" && ["botJoined", "botLeft", "botPassed", "botTimedOut"].contains(data.notice ?? "")
    }
}

extension HostContract.RoomConversation {
    public var isOpen: Bool { state == .open }

    /// Turns spent so far, kept inside the budget (a host that rounds oddly never shows "12 of 10").
    public var used: Int { Int(max(0, min(budget, budget - left))) }

    /// Whether this report is the same conversation's newer state than `old`. Events can arrive out of order and a
    /// refetch can land after a later event: an ended run never reopens, a run that spent more turns is the newer one.
    public func supersedes(_ old: RoomConversation) -> Bool {
        guard conversation == old.conversation else { return true }
        if isOpen != old.isOpen { return !isOpen }
        if !isOpen {
            // Both over: a reason beats none (an older host's end carries none), otherwise the later report stands.
            if old.endedWhy == nil || old.endedWhy == .unknown, endedWhy == nil || endedWhy == .unknown { return true }
            return endedWhy != nil && endedWhy != .unknown
        }
        return left <= old.left
    }

    /// Why it is over, with a Stop read from the state when the host gave no reason.
    var why: HostContract.RoomConversationEnd? {
        if let endedWhy, endedWhy != .unknown { return endedWhy }
        return state == .stopped ? .stopped : endedWhy
    }

    /// The quiet line where a run ends; nil for the ends that need none (a quiet day, a newer message) and for an old
    /// end the host kept no reason for.
    public var endLine: RoomEndLine? {
        let of = "\(used) of \(Turns.words(Int(budget)))"
        switch why {
        case .spent:
            return RoomEndLine(text: budget == 1 ? "Used its 1 turn" : "Used all \(budget) turns", offersMore: true)
        case .quiet:
            return RoomEndLine(text: "Worked it out · \(of)", offersMore: false)
        case .stopped:
            return RoomEndLine(text: "You stopped it · \(of) used", offersMore: false)
        default:
            return nil
        }
    }

    /// The glass line while the run is open.
    public func liveLine(now: Date = .now) -> RoomLiveLine? {
        guard isOpen else { return nil }
        let title: String
        switch current?.kind {
        case .bot?:
            let name = current?.name ?? "A bot"
            title = waitUntil.map { "\(name) · \(RoomLiveLine.wait(until: $0, now: now))" } ?? "\(name) · answering"
        case .agent?:
            title = "\(current?.name ?? "An agent") is answering"
        case nil:
            title = "Working it out"
        }
        let fraction = budget > 0 ? Double(used) / Double(budget) : 0
        return RoomLiveLine(title: title, detail: "\(max(0, left)) of \(budget) turns left", fraction: fraction,
                            botTurn: current?.kind == .bot)
    }
}

/// "You stopped it · 4 of 10 turns used", and for a run that spent its turns the button to give more.
public struct RoomEndLine: Hashable, Sendable {
    public var text: String
    public var offersMore: Bool

    /// "Give them 10 more": sends "Keep going" with this many turns.
    public static let moreTurns = Turns.standard
    public static var moreLabel: String { "Give them \(moreTurns) more" }
    public static let moreText = "Keep going"
}

public struct RoomLiveLine: Hashable, Sendable {
    public var title: String
    public var detail: String
    /// 0...1, the share of the turns already spent.
    public var fraction: Double
    public var botTurn: Bool

    /// "waits up to 5 min": how long the room waits for an outside bot's turn.
    public static func wait(until ms: Int64, now: Date) -> String {
        let left = Double(ms) / 1000 - now.timeIntervalSince1970
        guard left > 0 else { return "waiting for it" }
        return "waits up to \(max(1, Int((left / 60).rounded(.up)))) min"
    }
}

/// The conversations the app has heard of, by id, with the order they were first told. Newest report of one
/// conversation wins (`supersedes`), so a late-arriving older event changes nothing.
public struct RoomConversations: Equatable, Sendable {
    private(set) var byId: [String: RoomConversation] = [:]
    private var order: [String: Int] = [:]
    private var counter = 0

    public init() {}

    public var all: [RoomConversation] { byId.values.sorted { (order[$0.conversation] ?? 0) < (order[$1.conversation] ?? 0) } }

    /// Takes a report; false when it was older than what is held.
    @discardableResult
    public mutating func note(_ c: RoomConversation, newest: Bool = false) -> Bool {
        if let old = byId[c.conversation] {
            guard c.supersedes(old) else { return false }
        } else {
            counter += 1
            order[c.conversation] = counter
        }
        // A history answer names the lane's latest conversation: it is the newest even if its id was heard first.
        if newest, byId[c.conversation] != nil { counter += 1; order[c.conversation] = counter }
        byId[c.conversation] = c
        return true
    }

    public func conversation(_ id: String) -> RoomConversation? { byId[id] }

    private func inLane(_ chat: String, _ lane: String?) -> [RoomConversation] { all.filter { $0.chat == chat && $0.lane == lane } }

    /// The run in progress in a lane, if any (the last told, should the host ever say two).
    public func open(chat: String, lane: String?) -> RoomConversation? { inLane(chat, lane).last { $0.isOpen } }

    /// The newest conversation of a lane, open or over.
    public func latest(chat: String, lane: String?) -> RoomConversation? { inLane(chat, lane).last }

    /// Every run of the lane that is over.
    public func ended(chat: String, lane: String?) -> [RoomConversation] { inLane(chat, lane).filter { !$0.isOpen } }
}

extension HostContract.RoomConversation {
    /// The `conversation` event carries the same fields under a `type`.
    public init(_ e: HostContract.ConversationEvent) {
        self.init(conversation: e.conversation, chat: e.chat, lane: e.lane, budget: e.budget, left: e.left, state: e.state,
                  endedWhy: e.endedWhy, current: e.current, waitUntil: e.waitUntil, opening: e.opening)
    }
}

// MARK: adding a bot to a room

extension BotGuest {
    /// Why this bot can't be put in a room; nil when it can. Only the owner's own active bots join (a person's bot
    /// reads what the owner's rooms say, and the host refuses another person's).
    public var roomBlock: String? {
        if ownerPerson != nil { return "\(ownerName?.isEmpty == false ? ownerName! : "Someone")'s bot · only your own bots join your rooms" }
        switch state {
        case .active: return nil
        case .pending: return "Waiting for you to let it in"
        case .invited, .joining: return "Not connected yet"
        case .denied, .revoked, .unknown: return "Not available"
        }
    }

    /// The owner's bots to offer in the Add sheet: every one still in play (ended ones are gone), joinable or not.
    public static func roomCandidates(_ all: [BotGuest]) -> [BotGuest] { all.filter { !$0.state.isEnded } }

    /// "waits up to 5 min": how long its room waits for it on its turn (the host's default is five minutes).
    public var waitWords: String {
        let seconds = limits?.turnTimeoutS ?? 300
        return seconds < 90 ? "waits up to \(seconds) s" : "waits up to \(Int((Double(seconds) / 60).rounded(.up))) min"
    }

    /// Whether the host wakes it on its turn.
    public var isWoken: Bool { limits?.wakeProfile?.isEmpty == false }
}
