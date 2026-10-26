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

    /// Whether a task shows inline rather than as a card: while it works or waits on the user, and the newest one
    /// until it has been idle for ten minutes. A choice the user made on this device wins.
    public static func isOpen(_ status: TaskStatus, newest: Bool, lastAt: Int64, now: Int64, choice: Bool? = nil) -> Bool {
        if let choice { return choice }
        return status.isBusy || (newest && now - lastAt < idleMs)
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

extension Entry {
    /// The first message of a task, in the main chat.
    public var isTask: Bool { kind == "user" && threadId == nil && data.task == true }
}
