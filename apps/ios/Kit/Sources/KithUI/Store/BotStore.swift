import KithKit
import Foundation
import Observation

/// Opens the screen viewer, optionally watching a bot that's using the computer.
public struct ScreenRequest: Identifiable, Hashable, Sendable {
    public var watching: String?
    public var id: String { watching ?? "computer" }
    public init(watching: String? = nil) { self.watching = watching }
}

/// Where a message from this phone is: on its way, waiting in the relay mailbox for an offline
/// computer, accepted by the computer, or not sent.
public enum SendProgress: Sendable {
    case sending, queued, delivered, failed, cancelled
}

/// One computer's mirror (bots, transcripts) for a client (iPhone app, Mac window), kept by one
/// events stream (catch-up since `rev`, then live) over the encrypted channel.
/// `AccountStore` holds one per computer.
@MainActor
@Observable
public final class BotStore {
    public enum Connection: Equatable {
        case unpaired
        case connecting
        case online
        /// The relay says the computer isn't connected; messages can wait in its mailbox.
        case computerOffline(lastSeen: Date?)
        /// Can't reach the computer at all (no direct route, no relay).
        case offline(String)
        /// Revoked, lease expired, or the computer's identity changed.
        case unauthorized(String)
    }

    public private(set) var computer: Computer
    public internal(set) var connection: Connection = .connecting
    /// The first catch-up has arrived: everything up to the host's rev when this app connected.
    public internal(set) var caughtUp = false
    /// A thread to open once its chat is on screen (the bot's page sends the owner to a conversation); the chat clears it.
    public var pendingThread: PendingThread?
    var catchUpRev = Int64.max
    /// The `since` this connection's events stream was opened with; the `hello` that opens its
    /// catch-up reads it (after a mirror reset `rev` no longer says what was requested).
    var requestedSince: Int64 = 0
    /// Per bot, the lowest synced main-chat `seq` held when this connection's catch-up began
    /// (empty when it began from rev 0). See `isOutsideLoadedWindow`.
    var windowFloors: [String: Int64] = [:]
    static let dropGrace: Duration = .seconds(5)
    static let initialConnectionGrace: Duration = .seconds(1)
    var heldDrop: Connection?
    var dropTimer: Task<Void, Never>?
    /// How long an action waits for a reconnect before it gives up.
    static let actionPatience: Duration = .seconds(20)
    /// Actions waiting for the link to come back.
    var waiting = 0
    public internal(set) var hostRoute: HostRoute?
    public internal(set) var client: HostClient?
    /// Set on a computer this account joined by invite: shows the invite token on every connection that hasn't yet.
    @ObservationIgnored public var inviteTokens: (any InviteTokenStore)?
    /// Agents ringing the person right now (see `IncomingCalls`).
    public internal(set) var incomingCalls: [IncomingCall] = []
    /// Accepted calls already told to greet, so the agent speaks first only once.
    @ObservationIgnored var greeted: Set<String> = []
    public internal(set) var hello: Hello?
    /// Bots from outside (Grok and others) on this computer; see `BotGuestStore`.
    @ObservationIgnored public private(set) lazy var botGuests = BotGuestStore(store: self)
    /// The host's version and protocol levels, kept even when the rest of its `hello` can't be read.
    public internal(set) var hostVersion: HostVersion?
    public internal(set) var bots: [String: Bot] = [:]
    public internal(set) var entries: [String: [Entry]] = [:]
    /// Where each thread works, by `threadFolderKey`; filled by `loadThread` and `setPlaceFolder`.
    public internal(set) var threadFolders: [String: PlaceFolder] = [:]
    public internal(set) var usage: Usage
    /// The computer's remote screen (`nil`: the host predates it).
    public internal(set) var screen: ScreenState?
    /// Members' new devices waiting for the owner's OK (see `decideAccessRequest`); always empty for a member.
    public internal(set) var accessRequests: [MemberAccessRequest] = []
    /// Installed on the computer, for the Plugins screen and bot settings.
    public internal(set) var installedConnectors: [InstalledConnector] = []
    public internal(set) var installedSkills: [InstalledSkill] = []
    /// Bots whose older history has been fully paged in.
    public internal(set) var historyComplete: Set<String> = []
    var composerDrafts: [String: String] = [:]
    /// What was pasted into each draft, sent along so memory never takes it as the user's own words.
    @ObservationIgnored var composerPasted: [String: [String]] = [:]
    public var routineDrafts: [String: String] = [:]
    public let fileDownloads = FileDownloads()
    public var lastError: String?
    /// Bots whose thread the reader just left: the next message in their main chat starts a new task instead of
    /// going on with the current one (which would put it back in that thread).
    var leftThread: Set<String> = []
    /// What the user picked per task on this device: open or folded in the main chat; nil follows the task.
    var taskChoices: [String: Bool] = [:]
    /// Whether a send from the composer starts a new task: asked for, or (main chat only) the reader is out of the
    /// current task: they left its thread, or it is folded. `currentOpen` is nil when the chat has no task yet.
    func startsNewTask(_ botId: String, thread: String?, requested: Bool, currentOpen: Bool? = nil) -> Bool {
        if requested { return true }
        guard thread == nil else { return false }
        return leftThread.contains(botId) || currentOpen == false
    }
    /// Whether the bot's current task shows open in its main chat, by the rule TaskRow uses; nil without a task.
    func isCurrentTaskOpen(_ botId: String, now: Date = .now) -> Bool? {
        guard let bot = bots[botId], let root = Tasks.current(in: chat(botId)) else { return nil }
        let replies = self.replies(botId, root: root.id)
        let status = Tasks.status(of: root, bot: bot, replies: replies)
        return ThreadFeed.hasQuestion(replies, held: root.data.thread?.isHeldElsewhere == true)
            || Tasks.isOpen(status, newest: true, lastAt: Tasks.lastAt(root), now: Int64(now.timeIntervalSince1970 * 1000), choice: taskChoices[root.id])
    }
    /// The open conversation (navigation path).
    public var selection: String?
    /// The remote screen viewer (iPhone); `kith://screen` opens it.
    public var screenRequest: ScreenRequest?

    // Platform hooks (push registration, Live Activities, widgets).
    /// Each time the channel becomes ready.
    public var onConnected: (@MainActor (BotStore) -> Void)?
    public var onBotUpdated: (@MainActor (Bot) -> Void)?
    public var onUsageChanged: (@MainActor (ComputerID, Usage) -> Void)?
    /// A message to the bot from this phone moved along (the Live Activity follows it).
    public var onSent: (@MainActor (Bot, SendProgress) -> Void)?
    /// A bot appeared, changed or went away.
    var onRosterChanged: (@MainActor () -> Void)?
    /// The computer's addresses or keys changed (merged from its `hello`); persist it.
    var onComputerChanged: (@MainActor (Computer) -> Void)?

    public let storage: SharedStore.Context
    let makeTransport: @MainActor () async throws -> any HostTransport
    var transport: (any HostTransport)?
    var retired = false
    /// Files of messages not delivered yet (by client nonce), kept for a retry.
    @ObservationIgnored var outgoingFiles: [String: [OutgoingFile]] = [:]
    /// The folder chosen up front for messages not delivered yet (by client nonce), kept for a retry.
    @ObservationIgnored var outgoingStarts: [String: FolderChoice] = [:]
    /// Messages not delivered yet that start a task: a retry must start one too, not continue the current task.
    @ObservationIgnored var outgoingNewTasks: Set<String> = []

    var rev: Int64 = 0
    var hostId: String?
    var streamTask: Task<Void, Never>?
    var eventsTask: Task<Void, Never>?
    var isActive = true
    struct VoiceCall {
        let botId: String
        let startRev: Int64
        let speak: @MainActor (String) -> Void
        /// A notice from the computer (approval needed), as opposed to the bot's reply.
        let announce: @MainActor (String) -> Void
        /// The growing text of a reply still being written, `final` once it's done. Set by an
        /// engine that speaks replies as they stream in; those get no `speak` call.
        var stream: (@MainActor (String, String, Bool) -> Void)?
        /// The bot started or stopped working.
        var working: (@MainActor (Bool) -> Void)?
        let end: @MainActor () -> Void
    }
    @ObservationIgnored var voiceCalls: [UUID: VoiceCall] = [:]
    var saveTask: Task<Void, Never>?
    var rewound = false
    /// Events stayed undecodable after the rewind.
    var unreadable = false
    /// While one side needs an update there's nothing to sync; `hello` is asked again this often.
    static let mismatchRecheck: Duration = .seconds(30)

    /// Permission cards whose answer is on its way, with the chosen option: the card shows
    /// a spinner on it and takes no second answer.
    public internal(set) var answering: [String: String] = [:]
    /// Per session, how a Stop of the old run holding it stands (cards of one session share it). Nothing while idle.
    public internal(set) var holderPhases: [String: HeldRunModel.Phase] = [:]
    /// Per approved card, the old run its owner stopped from it: the transcript's "Stopped the old … run" divider.
    public internal(set) var stoppedRuns: [String: StoppedRun] = [:]

    @ObservationIgnored var readingViews: [UUID: ReadingScope] = [:]

    var cacheStamp: String?

    /// Loads this context's `DeviceIdentity` itself.
    public convenience init(computer: Computer, storage: SharedStore.Context) {
        self.init(computer: computer, storage: storage) {
            try await HostConnector.connect(computer, identity: try DeviceIdentity.load(context: storage))
        }
    }

    init(computer: Computer, storage: SharedStore.Context,
         transport: @escaping @MainActor () async throws -> any HostTransport) {
        self.computer = computer
        self.storage = storage
        makeTransport = transport
        usage = storage.usage[computer.id] ?? Usage()
        composerDrafts = storage.composerDrafts[computer.id] ?? [:]
        loadCache()
    }

    // MARK: derived

    /// Offline in any way: the computer is off, unreachable, or refuses this device.
    public var isOffline: Bool {
        switch connection {
        case .computerOffline, .offline, .unauthorized: true
        default: false
        }
    }

    /// Offline, but sends can wait in the relay mailbox until the computer is back.
    public var canQueue: Bool {
        // Only the warning is delayed; relay delivery can queue during that grace.
        if case .computerOffline = heldDrop ?? connection { computer.boxKey != nil && transport is any RemoteTransport } else { false }
    }

    /// What headers show: a reconnect the grace period keeps quiet reads "Connecting…"
    /// as soon as an action is waiting on it.
    public var shownConnection: Connection { waiting > 0 && connection == .online ? .connecting : connection }

    public var hostName: String { hello?.name ?? computer.name }

    /// Which side must update before this app and the host can work together
    /// (docs/reference/compatibility.md). `nil` until the host has answered, and whenever
    /// a version can't be read: an unknown is never a reason to lock someone out.
    public var mismatch: VersionMismatch? {
        guard let hostVersion else { return nil }
        if let found = VersionMismatch.check(hostVersion) { return found }
        // The host sends data this app can't read and speaks a newer protocol: this app is
        // behind, whatever the host's `minProtocol` says.
        if unreadable, (hostVersion.protocolLevel ?? 0) > VersionMismatch.protocolLevel {
            return .updateApp
        }
        return nil
    }

    /// Roster order: pinned first, then where the user dragged them, then most recent activity.
    public var roster: [Bot] {
        shownBots.filter { !$0.hidden }.sorted {
            if $0.pinned != $1.pinned { return $0.pinned }
            if $0.position != $1.position { return $0.position < $1.position }
            return $0.lastAt > $1.lastAt
        }
    }

    /// What this computer lets the person see: on one joined by invite, only the agents granted to them, whatever else
    /// the host sent (a taken-away agent leaves the list at once). An owner's hello has no list: all of it.
    var shownBots: [Bot] {
        guard let granted = hello?.grantedAgents else { return Array(bots.values) }
        let agents = Set(granted.map(\.agent))
        return bots.values.filter { $0.hqAgent.map(agents.contains) ?? false }
    }

    /// Cards outside this account's chats that wait for it on this computer (see `GuestPrompt`); `isOwner` is the
    /// account's role here.
    public func guestPrompts(isOwner: Bool) -> [GuestPrompt] { GuestPrompt.pending(in: entries, known: Set(bots.keys), isOwner: isOwner) }

    /// Display name for a harness id, as the host reports it (falls back to the built-in list).
    public func backendName(_ id: String) -> String {
        hello?.backends.first { $0.id == id }?.name ?? BackendInfo.name(id)
    }

    public var hiddenBots: [Bot] { bots.values.filter(\.hidden).sorted { $0.name < $1.name } }

    /// A bot's or group's main chat (thread replies are left out).
    public func chat(_ botId: String) -> [Entry] { (entries[botId] ?? []).filter { $0.threadId == nil } }

    /// Everything in a chat, threads included (the full conversation).
    public func allEntries(_ botId: String) -> [Entry] { entries[botId] ?? [] }

    /// A catch-up full of thread replies can leave only a few main-chat entries, or none.
    public func canLoadOlder(_ botId: String) -> Bool {
        !historyComplete.contains(botId) && allEntries(botId).contains { $0.seq > 0 && $0.seq != Int64.max }
    }

    /// The replies in the thread on `root`, oldest first.
    public func replies(_ botId: String, root: String) -> [Entry] { (entries[botId] ?? []).filter { $0.threadId == root } }

    /// A group's bots that still exist.
    public func members(of group: Bot) -> [Bot] { group.members.compactMap { bots[$0] } }

    /// Display name of an entry's author in a group chat.
    public func authorName(_ id: String?) -> String { id.flatMap { bots[$0]?.name } ?? "A deleted bot" }

    func updateComputer(_ change: (inout Computer) -> Void) {
        var c = computer
        change(&c)
        guard c != computer else { return }
        computer = c
        onComputerChanged?(c)
    }
}

/// A file picked in the composer, sent with the next message.
public struct OutgoingFile: Identifiable, Sendable {
    public let id = UUID()
    public let name: String
    public let data: Data

    public init(name: String, data: Data) {
        self.name = name
        self.data = data
    }

    /// Anything larger is refused by the computer.
    public static let maxSize = 100 * 1024 * 1024
}

/// A thread to open in a chat: the root message of one conversation.
public struct PendingThread: Equatable, Sendable {
    public var chat: String
    public var root: String

    public init(chat: String, root: String) {
        self.chat = chat
        self.root = root
    }
}
