import Foundation

// Tasks (host `chat/tasks.rs`): each burst of the user's chat with a bot is a task. Its first message is a root in
// the chat (`data.task`); the bot's answers and the rest of the burst are its thread. The chat shows a task inline
// while it's going and folds it into a card once it's idle.

public enum TaskStatus: Sendable {
    case working, needsYou, stopped, failed, done

    public var label: String {
        switch self {
        case .working: "Working"
        case .needsYou: "Needs you"
        case .stopped: "Stopped"
        case .failed: "Failed"
        case .done: "Done"
        }
    }

    public var isBusy: Bool { self == .working || self == .needsYou }
}

public enum Tasks {
    /// The host's `tasks::IDLE_MS`: a task idle this long is over, and folds.
    public static let idleMs: Int64 = 10 * 60_000

    /// The newest task in a chat: where a message without a thread goes on, and what the chat's Stop stops.
    public static func current(in chat: [Entry]) -> Entry? {
        chat.last(where: \.isTask)
    }

    /// The chat with its tasks' threads (other threads left out): what a voice call hears and reads.
    public static func chatAndTasks(_ all: [Entry]) -> [Entry] {
        let tasks = Set(all.filter(\.isTask).map(\.id))
        return all.filter { $0.threadId.map(tasks.contains) ?? true }
    }

    /// Where the task stands: at work (or waiting on an approval), stopped, failed, or done.
    public static func status(of root: Entry, bot: Bot, replies: [Entry]) -> TaskStatus {
        if bot.isWorking(in: bot.id, thread: root.id) {
            // A prompt in a thread another writer holds is read-only here, so it isn't ours to answer.
            let held = root.data.thread?.isHeldElsewhere == true
            return !held && replies.contains { $0.kind == "permission" && $0.data.status == "pending" } ? .needsYou : .working
        }
        // A session started in a terminal works outside the host's lanes; its thread says so.
        if root.data.thread?.isBusyInTerminal == true { return .working }
        // A message parked at its agent's monthly limit waits under a card with no lane at work.
        if replies.contains(where: { $0.kind == "permission" && $0.data.status == "pending" && $0.data.limit != nil }) { return .needsYou }
        // The host says how a finished turn ended; an older host doesn't, so the thread's entries tell. A host
        // "working" or "needsYou" while no lane is working is stale (the host restarted), so it isn't taken.
        switch root.data.thread?.status {
        case "stopped": return .stopped
        case "failed": return .failed
        case "done": return .done
        default: break
        }
        let last = replies.last { $0.kind == "notice" || ($0.kind == "agent" && $0.data.final == true) }
        return last?.kind == "notice" && last?.data.text == "Stopped." ? .stopped : .done
    }

    /// The thread row's subtitle: the status word, or the old run an approved card waits behind (no needs-you).
    public static func label(_ status: TaskStatus, replies: [Entry]) -> String {
        replies.contains(where: \.isHeldPermission) ? "Waiting for an old run" : status.label
    }

    /// When the task last moved: its newest message, or its start.
    public static func lastAt(_ root: Entry) -> Int64 {
        if let at = root.data.thread?.lastAt, at > 0 { return at }
        return root.createdAt
    }

    /// Whether a task's card shows its whole thread: the user's choice on this device, else while it works (so it
    /// folds when done).
    public static func isOpen(_ status: TaskStatus, choice: Bool? = nil) -> Bool {
        choice ?? (status == .working)
    }

    /// The main chat's order with tasks placed by their activity. A task keeps its root's place until it moves after
    /// the chat's newest other message (a terminal session started days ago, still talking): then it sits at the end,
    /// by its latest reply, where new things appear. `moved` are the ids of those tasks.
    public static func place(_ slots: [BotExchangeGroup.Item]) -> (slots: [BotExchangeGroup.Item], moved: Set<String>) {
        var latest: Int64 = 0
        for case .entry(let e) in slots where !e.isTask { latest = max(latest, e.createdAt) }
        for case .exchanges(let g) in slots { latest = max(latest, Int64(g.date.timeIntervalSince1970 * 1000)) }
        var moved: [(entry: Entry, at: Int64)] = []
        for case .entry(let e) in slots where e.isTask && lastAt(e) > latest { moved.append((e, lastAt(e))) }
        if moved.isEmpty { return (slots, []) }
        let ids = Set(moved.map(\.entry.id))
        let rest = slots.filter { if case .entry(let e) = $0 { return !ids.contains(e.id) } else { return true } }
        return (rest + moved.sorted { $0.at < $1.at }.map { .entry($0.entry) }, ids)
    }

    /// A card's title: the task's first message, on one line.
    public static func title(_ root: Entry) -> String {
        let text = (root.data.text ?? "").split(whereSeparator: \.isWhitespace).joined(separator: " ")
        return text.isEmpty ? "Task" : text
    }
}

/// What a folded task card hides: its last reply shows; the fold line counts the rest.
public struct TaskFold: Equatable, Sendable {
    /// The agent's newest final reply, shown under the fold line.
    public var last: Entry?
    /// Chat messages the fold hides (approval cards show on the card, so they aren't counted).
    public var messages: Int
    /// Tool steps the fold hides.
    public var steps: Int
}

/// A card's head line: the status word (none for done), and where the task came from, or who holds it now.
public struct TaskHead: Equatable, Sendable {
    public var status: String?
    public var origin: String?
    /// Someone other than the app holds the thread: the origin slot shows them as a chip.
    public var held: Bool
}

/// Where the chat's one box sends: a task a Reply picked, or a new task.
public enum TaskTarget: Equatable, Sendable {
    case task(String)
    case new
}

/// Where the box sends now: `going` is the task the Continues pill names (picked, else the one the host would go on
/// with); `into` is the thread the send carries (nil starts a new task); `picked` says the user chose it.
public struct TaskDestination: Equatable, Sendable {
    public var going: Entry?
    public var picked: Bool
    public var newTask: Bool
    public var into: Entry? { newTask ? nil : going }
}

extension Tasks {
    /// The fold of a task's thread: the last final reply, and what else is in it. `isStep` says which tool entries
    /// count as steps (a tool that delivered a file is not one).
    public static func fold(_ thread: [Entry], isStep: (Entry) -> Bool) -> TaskFold {
        let last = thread.last { $0.kind == "agent" && $0.data.final == true }
        var messages = 0, steps = 0
        for e in thread where e.id != last?.id {
            if e.kind == "tool" {
                if isStep(e) { steps += 1 }
            } else if e.isChat && e.kind != "permission" {
                messages += 1
            }
        }
        return TaskFold(last: last, messages: messages, steps: steps)
    }

    /// "2 messages · 2 steps", "1 step"; nil when the fold hides nothing.
    public static func foldLine(messages: Int, steps: Int) -> String? {
        var parts: [String] = []
        if messages > 0 { parts.append("\(messages) message\(messages == 1 ? "" : "s")") }
        if steps > 0 { parts.append("\(steps) step\(steps == 1 ? "" : "s")") }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }

    /// The origin words a card's head shows; the app's own tasks show none.
    public static func originLabel(_ origin: String?) -> String? {
        switch ThreadOrigin(origin) {
        case .app: nil
        case .terminal: "Terminal"
        case .telegram: "Telegram"
        case .discord: "Discord"
        case .slack: "Slack"
        case .bot: "Bot"
        case .other(let raw): raw.prefix(1).uppercased() + raw.dropFirst()
        }
    }

    /// A card's head: status (none when done), origin, and the writer holding it when that isn't the app.
    public static func head(_ status: TaskStatus, thread: ThreadSummary?, mac: String) -> TaskHead {
        let word: String? = status == .done ? nil : status.label
        if let thread, thread.isHeldElsewhere {
            return TaskHead(status: word, origin: thread.isHeldByTerminal ? "In terminal · \(mac)" : "Open elsewhere", held: true)
        }
        return TaskHead(status: word, origin: originLabel(thread?.origin), held: false)
    }

    /// The task the box goes on with when nothing is picked, as the host routes a message with no thread: the newest
    /// task while it works or for ten idle minutes. A terminal's own session is never gone on with by itself.
    public static func continuing(bot: Bot, chat: [Entry], now: Int64) -> Entry? {
        for e in chat.reversed() where e.isTask {
            if e.data.thread?.isTerminalOrigin == true { continue }
            return bot.isWorking(in: bot.id, thread: e.id) || now - lastAt(e) < idleMs ? e : nil
        }
        return nil
    }

    /// The task an entry belongs to: its thread, or itself when it is a task's first message.
    public static func of(_ entry: Entry) -> String? {
        entry.threadId ?? (entry.isTask ? entry.id : nil)
    }

    /// Where the box sends for a chat, given the target the user set (if any).
    public static func destination(target: TaskTarget?, bot: Bot, chat: [Entry], now: Int64) -> TaskDestination {
        var chosen: Entry?
        if case .task(let id) = target { chosen = chat.first { $0.id == id && $0.isTask } }
        let going = chosen ?? continuing(bot: bot, chat: chat, now: now)
        return TaskDestination(going: going, picked: chosen != nil, newTask: target == .new)
    }

    /// Whether sending into this task takes it over from a terminal first: only a task the user picked (a Reply),
    /// and only while the terminal is idle. A busy terminal, or a task the box went on with by itself, is sent to as
    /// the host routes it (queued until the terminal lets go).
    public static func takesOver(_ thread: ThreadSummary?, picked: Bool) -> Bool {
        picked && thread?.isHeldByTerminal == true && thread?.isBusyInTerminal != true
    }

    /// Sends into a task, taking it over first when `takesOver` says so; the message goes only once the take-over
    /// succeeded. nil when sent; else why the take-over failed, and nothing was sent (the caller keeps the text).
    @MainActor public static func sendIn(_ thread: ThreadSummary?, picked: Bool, takeOver: () async throws -> Void,
                              send: () async -> Void) async -> Error? {
        if takesOver(thread, picked: picked) {
            do { try await takeOver() } catch { return error }
        }
        await send()
        return nil
    }
}

extension Entry {
    /// The first message of a task, in the main chat.
    public var isTask: Bool { kind == "user" && threadId == nil && data.task == true }
}
