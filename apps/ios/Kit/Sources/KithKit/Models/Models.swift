import Foundation

// Wire types for the host (see host/src/api/). Field names match the
// host's camelCase JSON. Timestamps are epoch milliseconds.

public struct Bot: Codable, Identifiable, Hashable, Sendable {
    public var id: String
    /// `agent`, or `group`: several bots and the user in one chat (it has no agent, folder or harness).
    public var kind: String
    /// A group's bots.
    public var members: [String]
    public var name: String
    public var description: String
    /// `kith:<paletteId>` recolours the body; anything else (old ids like `blue` too) is the original.
    public var avatarColor: String
    /// Wire only: Kith's dot shapes are gone from rendering.
    public var avatarShape: String
    /// `auto` | `crew:<name>` | `spare:<name>` (an old `shape` means `auto`); a host that doesn't send it means `auto`.
    public var avatarSource: String = "auto"
    public var backend: String
    public var command: String?
    public var cwd: String
    /// Host-allocated personal workspace rather than a user-selected project.
    public var managedWorkspace: Bool = false
    /// The bot's default hq project; places with no pick of their own work there. Nil from an older host.
    public var project: String?
    /// The kith agent this bot is; only on bots made from a kith agent (host sends it, camelCase).
    public var hqAgent: String?
    /// Where the bot's own chat works; nil for a group or an older host.
    public var folder: PlaceFolder?
    /// `ask` or `auto`.
    public var permission: String
    public var model: String?
    /// The model's own settings (effort, fast…): agent option id → value.
    public var modelOptions: [String: String] = [:]
    /// The agent's Fast mode (Claude Code, Codex): faster replies for more usage.
    public var fastMode: Bool = false
    /// What the agent says about Fast mode: `unknown` (no session yet), `offered`, `unsupported`,
    /// or `refused` (it turned it off; `fastModeIssue` says why).
    public var fastModeSupport: String = "unknown"
    public var fastModeIssue: String?
    public var pinned: Bool
    /// Where the user dragged it, lowest first (after pinned bots); ties sort by recent activity.
    public var position: Int = 0
    public var hidden: Bool
    public var notify: Bool?
    /// Installed connector / skill ids this bot uses (see `Market.swift`).
    public var connectors: [String]
    public var skills: [String]
    /// The bot can see and operate the computer's desktop (the built-in `computer` tools).
    public var computer: Bool
    /// Uses the memory all the user's bots share ("About you"). Nil from an older host.
    public var sharedMemory: Bool?
    /// Keeps the harness's own memory (Claude Code's…) on beside Kith's. Nil where it has no switch.
    public var harnessMemory: Bool?
    /// Moves whenever memory changes, so an open memory screen fetches it again.
    public var memoryRev: Int64?
    /// Created without a name: the host names it after its first few conversations.
    public var autoName: Bool = false
    /// Each burst of the user's chat is a task with its own thread and session. Nil: on; false keeps one conversation.
    public var tasks: Bool?
    public var createdAt: Int64

    // Runtime, filled in by the host.
    public var rev: Int64
    /// idle | working | needsInput | error
    public var status: String
    public var activity: String
    public var startedAt: Int64?
    /// Where the running turn talks: the chat (a bot's own, or a group) and thread root.
    public var workingChat: String?
    public var workingThread: String?
    /// Every place it works in at once (its chat, threads, groups); nil from a host that runs one place at a time.
    public var workingLanes: [WorkingLane]? = nil
    public var unread: Int
    public var lastMessage: String?
    public var lastAt: Int64
    /// Set only on the chat a bot from outside (Grok…) has with an agent: the apps tag it and show it read-only.
    /// Nil on every other row and from an older host.
    public var botGuest: BotGuestInfo?

    public var isGroup: Bool { kind == "group" }
    public var isWorking: Bool { status == "working" || status == "needsInput" }
    /// Working right now in this chat's main conversation (`thread == nil`) or in that thread.
    public func isWorking(in chat: String, thread: String?) -> Bool {
        if let workingLanes { return workingLanes.contains(WorkingLane(chat: chat, thread: thread)) }
        return isWorking && (workingChat ?? id) == chat && workingThread == thread
    }
    public var needsInput: Bool { status == "needsInput" }
    /// The user's messages to it are tasks (a group is one room).
    public var usesTasks: Bool { !isGroup && tasks != false }
    public var folderName: String { managedWorkspace ? "Personal workspace" : (cwd as NSString).lastPathComponent }
}

/// A place a bot works in: a chat (its own or a group) and, in a thread, the thread's root.
public struct WorkingLane: Codable, Hashable, Sendable {
    public var chat: String
    public var thread: String?

    public init(chat: String, thread: String?) {
        self.chat = chat
        self.thread = thread
    }
}

/// Just enough of a `bot` event to detect deletions.
struct BotStub: Decodable {
    var id: String
    var deleted: Bool?
    var rev: Int64
}

public struct Entry: Codable, Identifiable, Hashable, Sendable {
    public var id: String
    public var seq: Int64
    /// The chat it's in: a bot's, or a group's.
    public var botId: String
    /// The root message of the thread it's in; nil in the main chat.
    public var threadId: String?
    public var rev: Int64
    /// user | agent | thought | tool | plan | permission | notice
    public var kind: String
    public var turn: Int64
    public var data: EntryData
    public var createdAt: Int64
    public var updatedAt: Int64

    public init(id: String, seq: Int64, botId: String, threadId: String? = nil, rev: Int64, kind: String, turn: Int64, data: EntryData, createdAt: Int64, updatedAt: Int64) {
        self.id = id
        self.seq = seq
        self.botId = botId
        self.threadId = threadId
        self.rev = rev
        self.kind = kind
        self.turn = turn
        self.data = data
        self.createdAt = createdAt
        self.updatedAt = updatedAt
    }

    /// Chat-visible (Grok Bot model): user messages, final replies, approval cards, notices. Of memory and context,
    /// only a fresh conversation gets a line: memory lives in the bot's memory settings, a summary keeps the same
    /// conversation going, and a kept handoff changed nothing.
    public var isChat: Bool {
        switch kind {
        case "user", "permission": true
        case "notice": data.memory == nil && data.memoryFile == nil && data.compaction == nil && data.handoff?.status != "kept"
        case "agent": data.final == true
        default: false
        }
    }

    public var date: Date { Date(milliseconds: createdAt) }
}

public struct ThreadSummary: Codable, Hashable, Sendable {
    public var count: Int
    public var lastAt: Int64
    /// Who replied, first reply first: bot ids, and `user`.
    public var authors: [String]
    /// Replies the user hasn't read (threads are read on their own).
    public var unread: Int?
    /// Where the conversation started: nil = in the app; `terminal`; later `bot`, `discord`, `telegram`, `slack`.
    /// Open on purpose: a value this app doesn't know decodes fine (see `ThreadOrigin`).
    public var origin: String?
    public var originLabel: String?
    /// A bot-guest thread: whose bot started it.
    public var originBot: OriginBot?
    /// Who may write now: nil = the app (or nobody), `terminal`, later `bot`. Open string; see `isHeldElsewhere`.
    public var liveWriter: String?
    /// The writer is mid-turn.
    public var liveBusy: Bool?
    public var terminal: TerminalSession?
    /// A task's status as the host says it: working, needsYou, stopped, failed, done. nil from an older host and a terminal's thread.
    public var status: String?
}

public struct OriginBot: Codable, Hashable, Sendable {
    public var provider: String?
    public var owner: String?
    public var name: String?
}

public struct TerminalSession: Codable, Hashable, Sendable {
    public var sessionId: String?
    public var cwd: String?
}

/// The origin words the apps know. Anything else is `.other` and still valid.
public enum ThreadOrigin: Hashable, Sendable {
    case app, terminal, bot, discord, telegram, slack
    case other(String)

    public init(_ raw: String?) {
        switch raw {
        case nil, "app": self = .app
        case "terminal": self = .terminal
        case "bot": self = .bot
        case "discord": self = .discord
        case "telegram": self = .telegram
        case "slack": self = .slack
        case let raw?: self = .other(raw)
        }
    }
}

public extension ThreadSummary {
    var threadOrigin: ThreadOrigin { ThreadOrigin(origin) }
    var isTerminalOrigin: Bool { origin == "terminal" }
    /// Who started a one-shot the host mirrored into the room ("asked by architect", "scheduled: nightly"); nil for a real terminal.
    var runLabel: String? {
        guard isTerminalOrigin, let label = originLabel?.trimmingCharacters(in: .whitespacesAndNewlines), !label.isEmpty else { return nil }
        return label
    }
    /// Someone other than the app holds the conversation, so the app shows it read-only. An unknown writer counts as held.
    var isHeldElsewhere: Bool { liveWriter.map { $0 != "app" } ?? false }
    /// A terminal is mid-turn here: work the host's own lanes don't show.
    var isBusyInTerminal: Bool { liveWriter == "terminal" && liveBusy == true }
    var isHeldByTerminal: Bool { liveWriter == "terminal" }
}

public extension Entry {
    /// For the root entry of a thread: the command that continues it in a terminal.
    func resumeCommand(agent: String) -> String { "kith \(agent) --resume \(id)" }
}

public struct FileDiff: Codable, Hashable, Sendable {
    public var path: String
    public var added: Int
    public var removed: Int
    public var isNew: Bool
    public var startLine: Int
    public var patch: String
}

public struct Location: Codable, Hashable, Sendable {
    public var path: String
    public var line: Int?
}

public struct PermissionOption: Codable, Hashable, Sendable, Identifiable {
    public var optionId: String
    public var name: String
    /// allow_once | allow_always | reject_once | reject_always
    public var kind: String
    public var id: String { optionId }
}

public struct PlanItem: Codable, Hashable, Sendable {
    public var content: String
    public var status: String
    public var priority: String?
}

public struct Usage: Codable, Hashable, Sendable {
    public var providers: [UsageProvider]
    public init(providers: [UsageProvider] = []) { self.providers = providers }
}

public struct UsageProvider: Codable, Hashable, Sendable, Identifiable {
    public var id: String
    public var name: String
    public var windows: [UsageWindow]
    public var source: String
    public var updatedAt: Int64
}

public struct UsageWindow: Codable, Hashable, Sendable, Identifiable {
    public var id: String
    public var label: String
    public var percent: Double
    public var resetsAt: Int64?
    /// Human text when only that is known ("Sep 26 at 12pm (Asia/Taipei)").
    public var resetsText: String?
    public var resetDate: Date? { resetsAt.map(Date.init(milliseconds:)) ?? resetsText.flatMap { Self.parseReset($0) } }

    /// Claude Code's text: "Sep 26 at 12pm (Asia/Taipei)", "Sep 25 at 7:30pm (Asia/Taipei)", or just "7:30pm (…)".
    /// The year isn't given: the next such date from `now`.
    static func parseReset(_ text: String, now: Date = .now) -> Date? {
        guard let match = text.firstMatch(of: /^(?:(\w{3} \d{1,2}) at )?(\d{1,2})(?::(\d{2}))?(am|pm)(?: \(([^)]+)\))?$/) else { return nil }
        var cal = Calendar(identifier: .gregorian)
        cal.timeZone = match.5.flatMap { TimeZone(identifier: String($0)) } ?? .current
        guard let hour12 = Int(match.2) else { return nil }
        var parts = cal.dateComponents([.year, .month, .day], from: now)
        if let day = match.1 {
            let f = DateFormatter()
            f.locale = Locale(identifier: "en_US_POSIX")
            f.dateFormat = "MMM d"
            guard let md = f.date(from: String(day)) else { return nil }
            let c = Calendar(identifier: .gregorian).dateComponents([.month, .day], from: md)
            parts.month = c.month
            parts.day = c.day
        }
        parts.hour = hour12 % 12 + (match.4 == "pm" ? 12 : 0)
        parts.minute = match.3.flatMap { Int($0) } ?? 0
        guard var date = cal.date(from: parts) else { return nil }
        // Past by more than a day: it means next year (or tomorrow, for a bare time).
        if date < now - 86_400 || (match.1 == nil && date < now) {
            date = cal.date(byAdding: match.1 == nil ? .day : .year, value: 1, to: date) ?? date
        }
        return date
    }

    /// "resets in 3h 20m" / "resets Sep 26 at 12pm", or nil.
    public var resetDescription: String? {
        if let d = resetDate { return "resets in \(RelativeTime.until(d))" }
        return resetsText.map { "resets \($0.replacingOccurrences(of: #" \(.*\)$"#, with: "", options: .regularExpression))" }
    }
}

public struct Backend: Codable, Hashable, Sendable, Identifiable {
    public var id: String
    public var name: String
    public var available: Bool
    public var command: String
    public var installHint: String
    /// ACP registry id, for the agent's icon (`agent-<id>` in the asset catalog).
    public var registry: String?
    public var description: String?
    public var installed: Bool?
    /// Known to Kith by name: it can install and sign in to it.
    public var curated: Bool?
    /// Missing when the host can't tell.
    public var signedIn: Bool?
    public var canInstall: Bool?
    /// Only for an agent that runs on Node.js, on a computer without its own: Kith's copy
    /// is `missing` or `added`.
    public var node: String?

    /// It can't run until Node.js is added to the computer.
    public var needsNode: Bool { node == "missing" }
}

/// How an agent can be signed in, from asking the agent itself.
public struct AgentAuth: Decodable, Sendable {
    public var signedIn: Bool?
    /// The agent's own words when it wants signing in (can hold a pairing code).
    public var detail: String?
    public var methods: [AuthMethod]
    /// Keys saved on the computer for this agent (names only).
    public var savedEnv: [String]?
    /// Kith's own sign-in command is available.
    public var login: Bool?
}

public struct AuthMethod: Decodable, Sendable, Identifiable, Hashable {
    public enum Kind: String, Decodable, Sendable {
        /// Runs in a setup terminal.
        case terminal
        /// The agent signs itself in (usually a browser on the computer).
        case agent
        /// Keys typed here, kept on the computer.
        case envVar
    }

    public struct Var: Decodable, Sendable, Hashable {
        public var name: String
        public var label: String
        public var secret: Bool
        public var optional: Bool
    }

    public var id: String
    public var name: String
    public var description: String?
    public var kind: Kind?
    public var vars: [Var]?
    public var link: String?
}

/// What a setup terminal on the computer runs.
public enum SetupStep: String, Codable, Sendable {
    case install, node, login
}

public struct Hello: Codable, Sendable {
    public var hostId: String
    public var name: String
    public var version: String
    public var os: String
    /// laptop · macmini · macstudio · imac · macpro · desktop · linux (missing from older hosts).
    public var device: String?
    public var home: String?
    public var backends: [Backend]
    public var rev: Int64
    /// Missing from hosts that predate remote screen.
    public var screen: ScreenState?
    /// The host's current direct addresses.
    public var urls: [String]?
    /// Channel identity (§9.4); missing only on loopback to an older host.
    public var computerId: ComputerID?
    public var signKey: String?
    public var boxKey: String?
    /// The host's protocol level and the oldest client level it serves (docs/reference/compatibility.md);
    /// both missing from a host before 0.1.0.
    public var protocolLevel: Int?
    public var minProtocol: Int?
    /// The cloud relay the host uses; nil = cloud off.
    public var cloud: String?
    /// For an invited person: the agents the owner gave them. Missing from a host before it, and from an owner's hello.
    public var grantedAgents: [GrantedAgent]?

    private enum CodingKeys: String, CodingKey {
        case grantedAgents, hostId, name, version, os, device, home, backends, rev, screen, urls, computerId, signKey, boxKey, protocolLevel = "protocol", minProtocol, cloud
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        hostId = try c.decode(String.self, forKey: .hostId)
        name = try c.decode(String.self, forKey: .name)
        version = try c.decode(String.self, forKey: .version)
        os = try c.decode(String.self, forKey: .os)
        device = try c.decodeIfPresent(String.self, forKey: .device)
        home = try c.decodeIfPresent(String.self, forKey: .home)
        // An invited person's hello leaves out the backends (and paths): a missing list is empty, a malformed one still fails.
        backends = try c.decodeIfPresent([Backend].self, forKey: .backends) ?? []
        rev = try c.decode(Int64.self, forKey: .rev)
        screen = try c.decodeIfPresent(ScreenState.self, forKey: .screen)
        urls = try c.decodeIfPresent([String].self, forKey: .urls)
        computerId = try c.decodeIfPresent(ComputerID.self, forKey: .computerId)
        signKey = try c.decodeIfPresent(String.self, forKey: .signKey)
        boxKey = try c.decodeIfPresent(String.self, forKey: .boxKey)
        protocolLevel = try c.decodeIfPresent(Int.self, forKey: .protocolLevel)
        minProtocol = try c.decodeIfPresent(Int.self, forKey: .minProtocol)
        cloud = try c.decodeIfPresent(String.self, forKey: .cloud)
        // Only a convenience for New chat: a list in a shape this version can't read is the same as none.
        grantedAgents = try? c.decodeIfPresent([GrantedAgent].self, forKey: .grantedAgents)
    }
}

/// One agent an invited person may use, from the member `hello`.
public struct GrantedAgent: Codable, Hashable, Sendable, Identifiable {
    public var agent: String
    public var name: String?
    public var role: InviteRole
    public var id: String { agent }
    public var title: String { name?.isEmpty == false ? name! : agent }

    public init(agent: String, name: String? = nil, role: InviteRole = .guest) {
        self.agent = agent
        self.name = name
        self.role = role
    }
}

public struct DirListing: Codable, Sendable {
    public var path: String
    public var parent: String?
    public var isGit: Bool
    /// The hq project registered at this folder.
    public var project: String?
    public var dirs: [DirItem]
    /// Home and each drive; absent from an older host.
    public var roots: [DirRoot]?
}

public struct DirItem: Codable, Hashable, Sendable, Identifiable {
    public var name: String
    public var path: String
    public var isGit: Bool
    public var project: String?
    public var id: String { path }
}

/// Create / update payload for a group chat.
public struct GroupDraft: Codable, Hashable, Sendable {
    public var id: String?
    public var kind = "group"
    public var name: String
    /// What the group is for; its bots are told.
    public var description = ""
    public var members: [String]
    public var pinned: Bool?

    public init(name: String, description: String = "", members: [String]) {
        self.name = name
        self.description = description
        self.members = members
    }

    public init(_ group: Bot) {
        id = group.id
        name = group.name
        description = group.description
        members = group.members
        pinned = group.pinned
    }
}

/// Create / update payload for a bot.
public struct BotDraft: Codable, Hashable, Sendable {
    public var id: String?
    public var name: String
    public var description: String
    public var avatarColor: String
    public var avatarShape: String
    /// Unset keeps the bot's; see `Bot.avatarSource`.
    public var avatarSource: String?
    public var backend: String
    public var command: String?
    public var cwd: String
    public var permission: String
    public var model: String?
    public var modelOptions: [String: String] = [:]
    public var fastMode: Bool?
    public var pinned: Bool?
    public var hidden: Bool?
    public var notify: Bool?
    public var connectors: [String]?
    public var skills: [String]?
    public var computer: Bool?
    /// The bot's default hq project. Always sent on an update: nil clears it.
    public var project: String?
    /// Only sent when the user changes it: unset, the host picks by the bot's folder.
    public var sharedMemory: Bool?
    /// Only sent when the user changes it.
    public var harnessMemory: Bool?

    public init(name: String = "", description: String = "", avatarColor: String = "",
                avatarShape: String = "blob", backend: String = "claude", cwd: String = "",
                permission: String = "auto") {
        self.name = name
        self.description = description
        self.avatarColor = avatarColor
        self.avatarShape = avatarShape
        avatarSource = "auto"
        self.backend = backend
        self.cwd = cwd
        self.permission = permission
    }

    public init(_ bot: Bot) {
        id = bot.id
        name = bot.name
        description = bot.description
        avatarColor = bot.avatarColor
        avatarShape = bot.avatarShape
        avatarSource = bot.avatarSource
        backend = bot.backend
        command = bot.command
        cwd = bot.managedWorkspace ? "" : bot.cwd
        permission = bot.permission
        model = bot.model
        modelOptions = bot.modelOptions
        fastMode = bot.fastMode
        pinned = bot.pinned
        hidden = bot.hidden
        notify = bot.notify
        connectors = bot.connectors
        skills = bot.skills
        computer = bot.computer
        project = bot.project
    }
}

public extension Date {
    init(milliseconds: Int64) { self.init(timeIntervalSince1970: Double(milliseconds) / 1000) }
}

extension Bot {
    /// Lenient: a host that is a little newer or older than the app still yields a usable bot.
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        kind = try c.decodeIfPresent(String.self, forKey: .kind) ?? "agent"
        members = try c.decodeIfPresent([String].self, forKey: .members) ?? []
        name = try c.decodeIfPresent(String.self, forKey: .name) ?? "Bot"
        description = try c.decodeIfPresent(String.self, forKey: .description) ?? ""
        avatarColor = try c.decodeIfPresent(String.self, forKey: .avatarColor) ?? ""
        avatarShape = try c.decodeIfPresent(String.self, forKey: .avatarShape) ?? "blob"
        avatarSource = try c.decodeIfPresent(String.self, forKey: .avatarSource) ?? "auto"
        backend = try c.decodeIfPresent(String.self, forKey: .backend) ?? "custom"
        command = try c.decodeIfPresent(String.self, forKey: .command)
        cwd = try c.decodeIfPresent(String.self, forKey: .cwd) ?? ""
        managedWorkspace = try c.decodeIfPresent(Bool.self, forKey: .managedWorkspace) ?? false
        project = try c.decodeIfPresent(String.self, forKey: .project)
        hqAgent = try c.decodeIfPresent(String.self, forKey: .hqAgent)
        folder = try? c.decodeIfPresent(PlaceFolder.self, forKey: .folder)
        permission = try c.decodeIfPresent(String.self, forKey: .permission) ?? "auto"
        model = try c.decodeIfPresent(String.self, forKey: .model)
        modelOptions = try c.decodeIfPresent([String: String].self, forKey: .modelOptions) ?? [:]
        fastMode = try c.decodeIfPresent(Bool.self, forKey: .fastMode) ?? false
        fastModeSupport = try c.decodeIfPresent(String.self, forKey: .fastModeSupport) ?? "unknown"
        fastModeIssue = try c.decodeIfPresent(String.self, forKey: .fastModeIssue)
        pinned = try c.decodeIfPresent(Bool.self, forKey: .pinned) ?? false
        position = try c.decodeIfPresent(Int.self, forKey: .position) ?? 0
        hidden = try c.decodeIfPresent(Bool.self, forKey: .hidden) ?? false
        notify = try c.decodeIfPresent(Bool.self, forKey: .notify)
        connectors = try c.decodeIfPresent([String].self, forKey: .connectors) ?? []
        skills = try c.decodeIfPresent([String].self, forKey: .skills) ?? []
        computer = try c.decodeIfPresent(Bool.self, forKey: .computer) ?? false
        sharedMemory = try c.decodeIfPresent(Bool.self, forKey: .sharedMemory)
        harnessMemory = try c.decodeIfPresent(Bool.self, forKey: .harnessMemory)
        memoryRev = try c.decodeIfPresent(Int64.self, forKey: .memoryRev)
        autoName = try c.decodeIfPresent(Bool.self, forKey: .autoName) ?? false
        tasks = try c.decodeIfPresent(Bool.self, forKey: .tasks)
        createdAt = try c.decodeIfPresent(Int64.self, forKey: .createdAt) ?? 0
        rev = try c.decodeIfPresent(Int64.self, forKey: .rev) ?? 0
        status = try c.decodeIfPresent(String.self, forKey: .status) ?? "idle"
        activity = try c.decodeIfPresent(String.self, forKey: .activity) ?? ""
        startedAt = try c.decodeIfPresent(Int64.self, forKey: .startedAt)
        workingChat = try c.decodeIfPresent(String.self, forKey: .workingChat)
        workingThread = try c.decodeIfPresent(String.self, forKey: .workingThread)
        workingLanes = try c.decodeIfPresent([WorkingLane].self, forKey: .workingLanes)
        unread = try c.decodeIfPresent(Int.self, forKey: .unread) ?? 0
        lastMessage = try c.decodeIfPresent(String.self, forKey: .lastMessage)
        lastAt = try c.decodeIfPresent(Int64.self, forKey: .lastAt) ?? createdAt
        botGuest = try? c.decodeIfPresent(BotGuestInfo.self, forKey: .botGuest)
    }
}
