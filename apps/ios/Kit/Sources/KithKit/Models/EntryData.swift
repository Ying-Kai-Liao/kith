import Foundation

public struct EntryData: Codable, Hashable, Sendable {
    public var connectionRequest: ConnectionRequest?
    /// A memory line an older host put in the chat. Memory lives in the bot's memory settings, so it isn't shown.
    public var memory: MemoryLine?
    public var memoryFile: MemoryLine?
    /// notice: the agent summarized its context.
    public var compaction: Compaction?
    /// notice: the bot's chat starts over with this handoff.
    public var handoff: HandoffCard?
    public var routineId: String?
    public var runId: String?
    public var text: String?
    public var final: Bool?
    /// agent: the backup chain entry that answered (`codex`, `claude:work`); nil when the agent's own account did.
    public var answeredBy: String?
    /// agent: how full the session is, in percent; the host sets it on a turn's last entry only when it should show.
    public var contextPercent: Int?
    /// user: queued | sent | cancelled | failed · tool: pending | in_progress | completed | failed · permission: pending | answered | cancelled | expired
    public var status: String?
    public var clientNonce: String?
    public var title: String?
    public var toolKind: String?
    public var output: String?
    public var diffs: [FileDiff]?
    public var locations: [Location]?
    public var options: [PermissionOption]?
    public var selected: String?
    /// permission, answered: the host's words for what became of the answer when it isn't the option's own (an "always"
    /// kith-auth couldn't keep stayed a once grant). Shown in place of the option's outcome.
    public var outcome: String?
    public var command: String?
    public var detail: String?
    public var cwd: String?
    /// permission, in an invited person's chat: who answers it, `owner` (a guest's) or `member`. Nil on the owner's own cards.
    public var approver: String?
    /// permission, in an invited person's chat: whose run asked, and which agent.
    public var person: String?
    public var personName: String?
    public var agent: String?
    public var entries: [PlanItem]?
    /// notice: info | error | divider
    public var style: String?
    /// Structured conversation data added to the existing team-tool notice.
    public var botMessage: BotMessage?
    public var heading: String?
    public var delegationId: String?
    public var sourceBotId: String?
    public var targetBotId: String?
    /// The bot that wrote it (a group shows who spoke).
    public var author: String?
    /// user, in a bot-guest chat: the outside bot that wrote it. Nil for the owner's own messages.
    public var sender: EntrySender?
    /// On a main-chat message that has a thread: its replies.
    public var thread: ThreadSummary?
    /// user, in the main chat: the first message of a task; the task goes on in its thread (see `Tasks`).
    public var task: Bool?
    /// The user's emoji reactions, oldest first.
    public var reactions: [String]?
    /// Files sent with a user message.
    public var attachments: [Attachment]?
    /// Immutable files explicitly sent by the bot.
    public var files: [SharedFile]?
    /// notice: a finished voice call's length.
    public var callSeconds: Int?
    /// notice: a voice call that began and may still be going; `callSeconds` replaces it at the end.
    public var callLive: Bool?
    /// notice (divider): who started or stopped writing. Open string; see `WriterChange`.
    public var writerChange: String?
    /// permission, answered: an old run still holds the session this approval resumes in (host `cards/resume.rs`). Nil
    /// otherwise, and on entries from an older host.
    public var held: HeldRun?
    /// notice: when the writer changed, in ms.
    public var at: Int64?
    /// permission: `account` (pick an account for a service) or `key` (type the key). Nil on every other card. Never a value.
    public var card: String?
    /// permission with `card`: what the agent needs a key for (host `cards/secret.rs`).
    public var secret: SecretRequest?
    /// permission with `card` `account`: the owner's accounts for that service, each with the agents holding it.
    public var accounts: [SecretAccount]?
    /// permission: an agent at its monthly limit (an owner's run waits under it) or a person at their budget
    /// (host `limits.rs`). Nil on every other card.
    public var limit: LimitCardData?

    public init(text: String? = nil, status: String? = nil, clientNonce: String? = nil) {
        self.text = text
        self.status = status
        self.clientNonce = clientNonce
    }
}

public struct EntrySender: Codable, Hashable, Sendable {
    public var botId: String?
    public var name: String?
    /// The person speaking through the bot, when it says.
    public var speaker: String?
}

/// A memory line from an older host, known only to keep it out of the chat.
public struct MemoryLine: Codable, Hashable, Sendable {}

/// A file sent with a message; the agent reads it on the computer.
public struct Attachment: Codable, Hashable, Sendable, Identifiable {
    public var id: String
    public var name: String
    public var size: Int64

    public init(id: String, name: String, size: Int64) {
        self.id = id
        self.name = name
        self.size = size
    }

    public var isImage: Bool {
        ["png", "jpg", "jpeg", "heic", "gif", "webp", "tiff", "bmp"].contains((name as NSString).pathExtension.lowercased())
    }
}

