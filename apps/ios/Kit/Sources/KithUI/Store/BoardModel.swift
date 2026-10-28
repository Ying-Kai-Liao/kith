import Foundation
import KithKit

// The Board beside Crew: what is waiting on you, what is going on, and what the crew is waiting for, derived from the
// stores the app already has. Nothing here is fetched or kept: change a store and the Board follows. The screen only
// lays these values out (apps/ios/Views/BoardView.swift).

/// A room on a computer: what a row opens.
public struct BoardTarget: Hashable, Sendable {
    public var computerId: ComputerID
    public var botId: String

    public init(computerId: ComputerID, botId: String) {
        self.computerId = computerId
        self.botId = botId
    }
}

/// What a computer that can't be reached right now is, for the chip on its items.
public enum BoardTrouble: Equatable, Sendable {
    case unreachable
    case catchingUp
}

/// What one computer gives the Board, as plain values copied from its store.
public struct BoardComputer: Sendable {
    public var id: ComputerID
    public var hostName: String
    public var trouble: BoardTrouble?
    /// The rooms this account sees there: on a joined computer only the agents granted to it.
    public var bots: [Bot]
    public var entries: [String: [Entry]]
    /// Guests' requests for this account's answer (owned computers only).
    public var guestPrompts: [GuestPrompt]
    /// Bots from outside waiting for this account's answer (owned computers only).
    public var botRequests: [BotGuest]

    public init(id: ComputerID, hostName: String, trouble: BoardTrouble? = nil, bots: [Bot], entries: [String: [Entry]],
                guestPrompts: [GuestPrompt] = [], botRequests: [BotGuest] = []) {
        self.id = id
        self.hostName = hostName
        self.trouble = trouble
        self.bots = bots
        self.entries = entries
        self.guestPrompts = guestPrompts
        self.botRequests = botRequests
    }

    @MainActor public init(store: BotStore, guestPrompts: [GuestPrompt], botRequests: [BotGuest] = []) {
        self.init(id: store.computer.id, hostName: store.hostName,
                  trouble: store.isOffline ? .unreachable : store.shownConnection == .connecting ? .catchingUp : nil,
                  bots: store.roster, entries: store.entries, guestPrompts: guestPrompts, botRequests: botRequests)
    }
}

/// A question or permission waiting on you, answerable in place with the chat's own call (`BotStore.respond`).
public struct BoardDecision: Identifiable, Equatable, Sendable {
    public var id: String
    public var target: BoardTarget
    public var hostName: String
    /// The chat's permission card; nil for a bot from outside asking to join.
    public var entry: Entry?
    /// The bot from outside this answers for (`BotGuestStore.decide`).
    public var botRequest: BotGuest?
    public var card: DecisionCardModel
    /// The bot's colour, for the card's tint.
    public var color: String
    public var at: Int64
    public var trouble: BoardTrouble?

    /// A key or account card: the Board only points at it (see `BoardModel.secretRow`).
    public var opensChat: Bool { entry?.data.isSecretCard == true }

    /// Bots from outside waiting for the owner's answer go first, then questions, oldest first.
    fileprivate var rank: Int { botRequest == nil ? 1 : 0 }
}

/// A task at work, or one finished today.
public struct BoardWork: Identifiable, Equatable, Sendable {
    public var id: String
    public var target: BoardTarget
    public var hostName: String
    public var bot: Bot
    public var title: String
    public var detail: String
    public var at: Int64
    public var trouble: BoardTrouble?
}

/// A bot whose chat started over with a handoff, shown as "Where we left off".
public struct BoardHandoff: Identifiable, Equatable, Sendable {
    public var id: String
    public var target: BoardTarget
    public var hostName: String
    public var botName: String
    public var card: HandoffCardModel
    public var at: Int64
}

/// Agents working in one project, by the place they work in.
public struct BoardProject: Identifiable, Equatable, Sendable {
    public var id: String
    public var name: String
    public var agents: Int
    public var working: Int
    /// The most recently active agent there.
    public var target: BoardTarget
}

/// Something the crew is waiting on.
public struct BoardWait: Identifiable, Equatable, Sendable {
    public enum Kind: Equatable, Sendable {
        /// Messages sent to a bot that wait for its turn.
        case queued(Int)
        /// A computer that can't be reached: what waits there waits for it.
        case computer(BoardTrouble)
    }

    public var id: String
    public var kind: Kind
    public var target: BoardTarget?
    public var hostName: String
    public var title: String
    public var detail: String
}

public struct BoardModel: Equatable, Sendable {
    public var needsYou: [BoardDecision] = []
    public var working: [BoardWork] = []
    public var doneToday: [BoardWork] = []
    public var leftOff: [BoardHandoff] = []
    public var projects: [BoardProject] = []
    public var waits: [BoardWait] = []

    /// How many show under Done today and Where we left off.
    public static let doneLimit = 5
    public static let leftOffLimit = 3
    /// A handoff older than this is no longer where anyone left off.
    public static let handoffAge: TimeInterval = 3 * 86_400
    /// A handoff's text can be a whole briefing; the card shows its start.
    static let noteLimit = 280

    public init() {}

    /// The total on the Board tab: questions waiting on you, on every computer.
    public var waiting: Int { needsYou.count }

    /// Nothing to show anywhere: the quiet empty state.
    public var isEmpty: Bool {
        needsYou.isEmpty && working.isEmpty && doneToday.isEmpty && leftOff.isEmpty && projects.isEmpty && waits.isEmpty
    }

    /// The room a Return in the dock opens: the first item in the order the Board shows them.
    public var top: BoardTarget? {
        // A bot from outside has no room to open.
        needsYou.first { $0.botRequest == nil }?.target ?? working.first?.target ?? doneToday.first?.target ?? leftOff.first?.target
            ?? waits.compactMap(\.target).first ?? projects.first?.target
    }

    public init(computers: [BoardComputer], now: Date = .now, calendar: Calendar = .current) {
        let nowMs = Int64(now.timeIntervalSince1970 * 1000)
        let dayStart = Int64(calendar.startOfDay(for: now).timeIntervalSince1970 * 1000)
        var decisions: [BoardDecision] = []
        var projectBots: [String: [(bot: Bot, computer: ComputerID)]] = [:]

        for computer in computers {
            // Bots from outside asking to join: no chat of the owner's either.
            for request in computer.botRequests {
                decisions.append(BoardDecision(
                    id: "\(computer.id)/bot/\(request.botId)", target: BoardTarget(computerId: computer.id, botId: request.botId),
                    hostName: computer.hostName, entry: nil, botRequest: request, card: DecisionCardModel(botRequest: request),
                    color: "", at: request.createdAt, trouble: computer.trouble))
            }
            // Guests' requests: outside any chat of the owner's, so no bot to open.
            for prompt in computer.guestPrompts {
                let target = BoardTarget(computerId: computer.id, botId: prompt.chatId)
                decisions.append(BoardDecision(
                    id: "\(computer.id)/\(prompt.id)", target: target, hostName: computer.hostName, entry: prompt.entry,
                    botRequest: nil, card: DecisionCardModel(guest: prompt), color: "", at: prompt.entry.createdAt, trouble: computer.trouble))
            }
            if let trouble = computer.trouble {
                waits.append(BoardWait(id: "computer/\(computer.id)", kind: .computer(trouble), target: nil, hostName: computer.hostName,
                                       title: computer.hostName, detail: trouble == .unreachable ? "Can't be reached right now" : "Catching up"))
            }
            for bot in computer.bots {
                let target = BoardTarget(computerId: computer.id, botId: bot.id)
                let entries = computer.entries[bot.id] ?? []
                var tasks: [Entry] = []
                var pending: [Entry] = []
                var queued = 0
                var handoff: Entry?
                for entry in entries {
                    if entry.isTask {
                        tasks.append(entry)
                    } else if Self.isWaiting(entry) {
                        pending.append(entry)
                    } else if entry.kind == "user", entry.data.status == "queued" {
                        queued += 1
                    } else if entry.kind == "notice", let card = entry.data.handoff, card.status != "kept",
                              !(card.text ?? "").trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
                              entry.createdAt > (handoff?.createdAt ?? 0) {
                        handoff = entry
                    }
                }
                let titles = Dictionary(tasks.map { ($0.id, Tasks.title($0)) }, uniquingKeysWith: { first, _ in first })

                for entry in pending {
                    var card = entry.data.isSecretCard ? Self.secretRow(entry, agent: bot.name) : Self.decision(for: entry)
                    card.heading = [bot.name, entry.threadId.flatMap { titles[$0] }].compactMap { $0 }.joined(separator: " · ")
                    decisions.append(BoardDecision(
                        id: "\(computer.id)/\(entry.id)", target: target, hostName: computer.hostName, entry: entry,
                        botRequest: nil, card: card,
                        color: bot.avatarColor, at: entry.createdAt, trouble: computer.trouble))
                }

                var taskIsWorking = false
                for task in tasks {
                    let lastAt = Tasks.lastAt(task)
                    let atWork = bot.isWorking(in: bot.id, thread: task.id) || task.data.thread?.isBusyInTerminal == true
                    guard atWork || lastAt >= dayStart else { continue }
                    let replies = entries.filter { $0.threadId == task.id }
                    switch Tasks.status(of: task, bot: bot, replies: replies) {
                    case .working:
                        taskIsWorking = true
                        working.append(BoardWork(id: "\(computer.id)/\(task.id)", target: target, hostName: computer.hostName, bot: bot,
                                                 title: Tasks.title(task), detail: Self.activity(bot), at: lastAt, trouble: computer.trouble))
                    case .needsYou:
                        // Its question is in Needs you.
                        taskIsWorking = true
                    case .done:
                        let said = replies.last { $0.kind == "agent" && $0.data.final == true }?.data.text
                        doneToday.append(BoardWork(id: "\(computer.id)/\(task.id)", target: target, hostName: computer.hostName, bot: bot,
                                                   title: Tasks.title(task), detail: said.map(MessagePreview.text) ?? bot.name,
                                                   at: lastAt, trouble: computer.trouble))
                    case .stopped, .failed:
                        break
                    }
                }
                // A bot at work in its main chat (no task of its own, a group's member, an older host).
                if !taskIsWorking, bot.isWorking(in: bot.id, thread: nil), !bot.needsInput {
                    working.append(BoardWork(id: "\(computer.id)/\(bot.id)", target: target, hostName: computer.hostName, bot: bot,
                                             title: bot.name, detail: Self.activity(bot), at: bot.startedAt ?? bot.lastAt,
                                             trouble: computer.trouble))
                }

                if let handoff, let text = handoff.data.handoff?.text,
                   nowMs - handoff.createdAt < Int64(Self.handoffAge * 1000) {
                    let clean = text.split(whereSeparator: \.isWhitespace).joined(separator: " ")
                    let note = clean.count > Self.noteLimit ? String(clean.prefix(Self.noteLimit)) + "…" : clean
                    leftOff.append(BoardHandoff(id: "\(computer.id)/\(handoff.id)", target: target, hostName: computer.hostName,
                                                botName: bot.name, card: HandoffCardModel(note: note), at: handoff.createdAt))
                }
                if queued > 0 {
                    waits.append(BoardWait(id: "queued/\(computer.id)/\(bot.id)", kind: .queued(queued), target: target,
                                           hostName: computer.hostName, title: bot.name,
                                           detail: queued == 1 ? "1 message waiting for its turn" : "\(queued) messages waiting for its turn"))
                }
                if !bot.isGroup, let folder = bot.folder, folder.kind == "project" {
                    projectBots[folder.project ?? folder.name, default: []].append((bot, computer.id))
                }
            }
        }

        needsYou = decisions.sorted { $0.rank != $1.rank ? $0.rank < $1.rank : $0.at != $1.at ? $0.at < $1.at : $0.id < $1.id }
        working.sort { $0.at != $1.at ? $0.at > $1.at : $0.id < $1.id }
        doneToday = Array(doneToday.sorted { $0.at != $1.at ? $0.at > $1.at : $0.id < $1.id }.prefix(Self.doneLimit))
        leftOff = Array(leftOff.sorted { $0.at > $1.at }.prefix(Self.leftOffLimit))
        // The computers first, then what waits on a bot.
        waits.sort { lhs, rhs in
            let l = lhs.target == nil, r = rhs.target == nil
            return l != r ? l : lhs.id < rhs.id
        }
        projects = projectBots.compactMap { (name, members) -> BoardProject? in
            guard let latest = members.max(by: { $0.bot.lastAt < $1.bot.lastAt }) else { return nil }
            return BoardProject(id: name, name: name, agents: members.count,
                                working: members.filter { $0.bot.isWorking }.count,
                                target: BoardTarget(computerId: latest.computer, botId: latest.bot.id))
        }
        .sorted { $0.working != $1.working ? $0.working > $1.working : $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
    }

    // MARK: pieces

    /// A permission card waiting on this account, as the host says (`canAnswer`). From an older host, a guest's card in
    /// a chat of this account's own is the owner's to answer, not this one's.
    static func isWaiting(_ entry: Entry) -> Bool {
        entry.kind == "permission" && entry.data.status == "pending" && (entry.data.canAnswer ?? (entry.data.approver != "owner"))
    }

    /// Questions waiting on you in one bot's chat: what its Crew row badges, the same count the Board adds up.
    public static func waitingCount(in entries: [Entry]) -> Int {
        entries.filter(isWaiting).count
    }

    private static func activity(_ bot: Bot) -> String {
        bot.activity.isEmpty ? "Working…" : bot.activity
    }

    /// An Access or Key card on the Board: one line that opens the chat at the card. Never an answer or a field here, so the
    /// key is only ever typed where the agent's card is. No options: the row is the whole control.
    static func secretRow(_ entry: Entry, agent: String) -> DecisionCardModel {
        let service = entry.data.secret?.service ?? "a service"
        return DecisionCardModel(question: "\(agent) needs a key for \(service)", options: [])
    }

    /// The chat's permission card as a decision card: what it wants, where, and only the answers it offered.
    static func decision(for entry: Entry) -> DecisionCardModel {
        // An agent at its monthly limit: "architect used its $300 for October. One run is waiting."
        if let question = Spend.question(entry) {
            var card = DecisionCardModel(guest: GuestPrompt(entry))
            card.question = question
            card.detail = nil
            return card
        }
        let headline = switch entry.data.toolKind {
        case "execute": "Wants to run a command"
        case "edit", "delete", "move": "Wants to change files"
        case "fetch": "Wants to access the web"
        case "read", "search": "Wants to read files"
        default: "Wants to use a tool"
        }
        var card = DecisionCardModel(guest: GuestPrompt(entry))
        card.question = headline
        card.detail = entry.data.title
        return card
    }

    // MARK: the dock

    /// The Board with only what `query` matches, each section ranked as Crew ranks rooms. Empty query: unchanged.
    public func filtered(by query: String) -> BoardModel {
        let query = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !query.isEmpty else { return self }
        var out = BoardModel()
        out.needsYou = Self.rank(needsYou, query) {
            CrewSearchEntry(id: $0.id, name: $0.card.question, people: [$0.card.heading ?? "", $0.hostName], status: $0.card.detail ?? "",
                            extras: $0.card.options.map(\.title))
        }
        out.working = Self.rank(working, query) {
            CrewSearchEntry(id: $0.id, name: $0.title, people: [$0.bot.name, $0.hostName], status: $0.detail)
        }
        out.doneToday = Self.rank(doneToday, query) {
            CrewSearchEntry(id: $0.id, name: $0.title, people: [$0.bot.name, $0.hostName], status: $0.detail)
        }
        out.leftOff = Self.rank(leftOff, query) {
            CrewSearchEntry(id: $0.id, name: $0.botName, people: [$0.hostName], status: $0.card.note ?? "")
        }
        out.projects = Self.rank(projects, query) { CrewSearchEntry(id: $0.id, name: $0.name) }
        out.waits = Self.rank(waits, query) {
            CrewSearchEntry(id: $0.id, name: $0.title, people: [$0.hostName], status: $0.detail)
        }
        return out
    }

    private static func rank<T: Identifiable>(_ items: [T], _ query: String, _ entry: (T) -> CrewSearchEntry) -> [T] where T.ID == String {
        let byId = Dictionary(items.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
        return CrewSearch.rank(items.map(entry), query: query).compactMap { byId[$0] }
    }
}
