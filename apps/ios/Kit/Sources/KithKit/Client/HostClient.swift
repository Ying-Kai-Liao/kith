import Foundation

public enum HostEvent: Sendable {
    case hello(hostId: String, rev: Int64, usage: Usage, screen: ScreenState?)
    case bot(Bot)
    case botDeleted(id: String, rev: Int64)
    case entry(Entry)
    case usage(Usage)
    case screen(ScreenState)
    case resync
    /// An agent is calling (ringing), or a call stopped ringing (answered, declined, missed).
    case call(IncomingCallEvent)
    /// A bot guest is waiting for the owner's answer (`botRequest`), or one changed (`botUpdated`). Neither carries what
    /// the screens need: they read the lists again.
    case botRequest
    case botUpdated
    /// An event this app version couldn't decode (host newer/older than the app).
    case undecodable(type: String)
}

/// A setup terminal's output stream.
public enum TermEvent: Sendable {
    case output(Data)
    case exit(Int)
}

/// A `thread` response: the replies and, from a host that has place folders, where the thread works.
struct ThreadPage: Decodable {
    var entries: [Entry]
    var folder: PlaceFolder?

    private enum CodingKeys: String, CodingKey { case entries, folder }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        entries = try c.decode([Entry].self, forKey: .entries)
        folder = try? c.decodeIfPresent(PlaceFolder.self, forKey: .folder)
    }
}

public struct Empty: Codable, Sendable {
    public init() {}
}

/// A full editor update must explicitly clear a previous model override.
struct BotUpdate: Encodable {
    let draft: BotDraft
    private enum CodingKeys: String, CodingKey { case model, project }

    func encode(to encoder: Encoder) throws {
        try draft.encode(to: encoder)
        var container = encoder.container(keyedBy: CodingKeys.self)
        if draft.model == nil { try container.encodeNil(forKey: .model) }
        // The host reads null as "no default project".
        if draft.project == nil { try container.encodeNil(forKey: .project) }
    }
}

/// One setting of a bot. The host merges the keys it gets, so a toggle sends only its own key and
/// can't revert changes made elsewhere from this device's possibly stale copy of the bot.
struct BotSetting<Value: Encodable & Sendable>: Encodable {
    let id: String
    let key: String
    let value: Value

    private struct Key: CodingKey {
        let stringValue: String
        var intValue: Int? { nil }
        init(_ name: String) { stringValue = name }
        init?(stringValue: String) { self.stringValue = stringValue }
        init?(intValue: Int) { nil }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: Key.self)
        try container.encode(id, forKey: Key("id"))
        try container.encode(value, forKey: Key(key))
    }
}

extension Error {
    /// The task was cancelled (a view went away), which is no failure to show.
    public var isCancellation: Bool {
        self is CancellationError || (self as? URLError)?.code == .cancelled
    }
}

public struct HostClient: Sendable {
    public let transport: any HostTransport

    public init(transport: any HostTransport) {
        self.transport = transport
    }

    static let decoder = JSONDecoder()
    static let encoder = JSONEncoder()

    public func call<T: Decodable>(_ method: String, _ body: some Encodable = Empty(), timeout: TimeInterval = 20) async throws -> T {
        let data = try await transport.call(method, body: try Self.encoder.encode(body), timeout: timeout)
        return try Self.decoder.decode(T.self, from: data)
    }

    /// Catch-up since `since`, then live updates.
    public func events(since: Int64, client: String) -> AsyncThrowingStream<HostEvent, Error> {
        map(transport.stream(.events(since: since, client: client)), Self.parseEvent)
    }

    /// A setup terminal's output: everything so far, then live, finishing after `.exit`.
    public func termOutput(_ term: String) -> AsyncThrowingStream<TermEvent, Error> {
        map(transport.stream(.term(term))) { data in
            struct Raw: Decodable { var type: String; var data: String?; var code: Int? }
            guard let raw = try? Self.decoder.decode(Raw.self, from: data) else { return nil }
            switch raw.type {
            case "output": return raw.data.flatMap { Data(base64Encoded: $0) }.map(TermEvent.output)
            case "exit": return .exit(raw.code ?? -1)
            default: return nil
            }
        }
    }

    private func map<T: Sendable>(_ source: AsyncThrowingStream<Data, Error>, _ parse: @escaping @Sendable (Data) -> T?) -> AsyncThrowingStream<T, Error> {
        AsyncThrowingStream { continuation in
            let task = Task {
                do {
                    for try await data in source {
                        if let event = parse(data) { continuation.yield(event) }
                    }
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    public static func parseEvent(_ data: Data) -> HostEvent? {
        struct Envelope: Decodable {
            var type: String
            var hostId: String?
            var rev: Int64?
        }
        struct BotWrap: Decodable { var bot: Bot }
        struct StubWrap: Decodable { var bot: BotStub }
        struct EntryWrap: Decodable { var entry: Entry }
        struct UsageWrap: Decodable { var usage: Usage }
        struct ScreenWrap: Decodable { var screen: ScreenState }
        let d = decoder
        guard let env = try? d.decode(Envelope.self, from: data) else { return nil }
        switch env.type {
        case "hello":
            let usage = (try? d.decode(UsageWrap.self, from: data))?.usage ?? Usage()
            let screen = (try? d.decode(ScreenWrap.self, from: data))?.screen
            return .hello(hostId: env.hostId ?? "", rev: env.rev ?? 0, usage: usage, screen: screen)
        case "bot":
            if let stub = try? d.decode(StubWrap.self, from: data), stub.bot.deleted == true {
                return .botDeleted(id: stub.bot.id, rev: stub.bot.rev)
            }
            return (try? d.decode(BotWrap.self, from: data)).map { .bot($0.bot) } ?? .undecodable(type: "bot")
        case "entry":
            return (try? d.decode(EntryWrap.self, from: data)).map { .entry($0.entry) } ?? .undecodable(type: "entry")
        case "usage":
            return (try? d.decode(UsageWrap.self, from: data)).map { .usage($0.usage) }
        case "screen":
            return (try? d.decode(ScreenWrap.self, from: data)).map { .screen($0.screen) }
        case "resync":
            return .resync
        case "call":
            return IncomingCallEvent.decode(data).map { .call($0) }
        case "botRequest":
            return .botRequest
        case "botUpdated":
            return .botUpdated
        default:
            return nil
        }
    }
}

// MARK: - Typed calls

public extension HostClient {
    func hello() async throws -> Hello { try await call("hello") }

    func history(botId: String, beforeSeq: Int64, limit: Int = 100) async throws -> [Entry] {
        struct Body: Encodable { var botId: String; var beforeSeq: Int64; var limit: Int }
        struct Res: Decodable { var entries: [Entry] }
        let res: Res = try await call("history", Body(botId: botId, beforeSeq: beforeSeq, limit: limit))
        return res.entries
    }

    /// A thread's replies (the root is in the main chat).
    func thread(botId: String, rootId: String) async throws -> [Entry] {
        try await threadPage(botId: botId, rootId: rootId).entries
    }

    /// A thread's replies and the folder it works in (nil from an older host).
    func threadPage(botId: String, rootId: String) async throws -> (entries: [Entry], folder: PlaceFolder?) {
        let res: ThreadPage = try await call("thread", ["botId": botId, "rootId": rootId])
        return (res.entries, res.folder)
    }

    /// The newest 200 bot-to-bot notices between a bot's chat and a peer, oldest first.
    func botConversation(botId: String, peerId: String) async throws -> [Entry] {
        struct Res: Decodable { var entries: [Entry] }
        let res: Res = try await call("botConversation", ["botId": botId, "peerId": peerId])
        return res.entries
    }

    /// Creates a group chat (or returns the one these bots already share).
    func createGroup(_ draft: GroupDraft) async throws -> Bot {
        struct Res: Decodable { var bot: Bot }
        let res: Res = try await call("createBot", draft)
        return res.bot
    }

    func updateGroup(_ draft: GroupDraft) async throws -> Bot {
        struct Res: Decodable { var bot: Bot }
        let res: Res = try await call("updateBot", draft)
        return res.bot
    }

    func createBot(_ draft: BotDraft) async throws -> Bot {
        struct Res: Decodable { var bot: Bot }
        let res: Res = try await call("createBot", draft)
        return res.bot
    }

    func updateBot(_ draft: BotDraft) async throws -> Bot {
        struct Res: Decodable { var bot: Bot }
        let res: Res = try await call("updateBot", BotUpdate(draft: draft))
        return res.bot
    }

    func updateBotSetting<Value: Encodable & Sendable>(_ id: String, _ key: String, _ value: Value) async throws {
        let _: Empty = try await call("updateBot", BotSetting(id: id, key: key, value: value))
    }

    func deleteBot(_ id: String) async throws {
        let _: Empty = try await call("deleteBot", ["botId": id])
    }

    /// The roster as the user dragged it, top first.
    func reorderBots(_ ids: [String]) async throws {
        let _: Empty = try await call("reorderBots", ["ids": ids])
    }

    /// The main chat, one thread (`threadId`), or everything (`all`).
    func markRead(_ id: String, threadId: String? = nil, all: Bool = false) async throws {
        struct Body: Encodable { var botId: String; var threadId: String?; var all: Bool }
        let _: Empty = try await call("markRead", Body(botId: id, threadId: threadId, all: all))
    }

    /// Toggles the user's `emoji` reaction on a chat message.
    func react(entryId: String, emoji: String) async throws -> Entry {
        struct Res: Decodable { var entry: Entry }
        let res: Res = try await call("react", ["entryId": entryId, "emoji": emoji])
        return res.entry
    }

    /// Takes back a message that still waits for its turn; the host marks it cancelled.
    func unsend(botId: String, entryId: String) async throws -> Entry {
        struct Res: Decodable { var entry: Entry }
        let res: Res = try await call("unsend", ["botId": botId, "entryId": entryId])
        return res.entry
    }

    /// `threadId`: reply in the thread on that main-chat message.
    /// `pasted`: what the user pasted rather than typed, which memory never takes as their own words.
    /// `newTask`: start a task of its own rather than go on with the current one.
    /// `start`: the project or folder the place this message runs in starts in.
    /// `via`: "call" for a message spoken in a voice call (an older computer ignores it).
    func send(botId: String, text: String, clientNonce: String, threadId: String? = nil, attachments: [String]? = nil,
              pasted: [String]? = nil, newTask: Bool = false, start: FolderChoice? = nil, via: String? = nil) async throws -> Entry {
        struct Body: Encodable {
            var botId: String; var text: String; var clientNonce: String; var threadId: String?; var attachments: [String]?
            var pasted: [String]?; var newTask: Bool?; var project: String?; var folder: String?; var via: String?
        }
        struct Res: Decodable { var entry: Entry }
        let body = Body(botId: botId, text: text, clientNonce: clientNonce, threadId: threadId, attachments: attachments,
                        pasted: pasted?.isEmpty == false ? pasted : nil, newTask: newTask ? true : nil,
                        project: start?.projectId, folder: start?.folderPath, via: via)
        do {
            let res: Res = try await call("send", body)
            return res.entry
        } catch {
            // A thread another writer holds: the app shows "Working in terminal", not a failed send.
            throw ThreadWriterError(error) ?? error
        }
    }

    /// Bytes per `upload` call: base64 of it stays well under the channel's 1 MiB message.
    static let uploadChunk = 384 * 1024

    /// Sends one file in chunks for a later `send`; `id` names the upload there.
    func upload(botId: String, id: String, name: String, data: Data) async throws {
        struct Body: Encodable { var botId, uploadId, name, data: String; var offset: Int; var done: Bool }
        var offset = 0
        repeat {
            let end = min(offset + Self.uploadChunk, data.count)
            let chunk = data.subdata(in: offset..<end).base64EncodedString()
            let _: Empty = try await call("upload", Body(botId: botId, uploadId: id, name: name, data: chunk, offset: offset, done: end == data.count), timeout: 60)
            offset = end
        } while offset < data.count
    }

    /// A sent file, fetched back in chunks.
    func readUpload(botId: String, id: String) async throws -> Data {
        struct Body: Encodable { var botId, uploadId: String; var offset: Int }
        struct Res: Decodable { var data: String; var size: Int }
        var out = Data()
        repeat {
            let res: Res = try await call("readUpload", Body(botId: botId, uploadId: id, offset: out.count), timeout: 60)
            guard let chunk = Data(base64Encoded: res.data), !chunk.isEmpty else { break }
            out.append(chunk)
            if out.count >= res.size { break }
        } while true
        return out
    }

    /// Answers an incoming call (`id` from its event). The first device to answer wins; the others get an error.
    func callAccept(_ id: String) async throws {
        let _: Empty = try await call("callAccept", ["id": id])
    }

    func callDecline(_ id: String) async throws {
        let _: Empty = try await call("callDecline", ["id": id])
    }

    /// The accepted call is live: the agent speaks first.
    func callGreet(_ id: String) async throws {
        let _: Empty = try await call("callGreet", ["id": id])
    }

    func logCall(_ botId: String, seconds: Int) async throws {
        struct Body: Encodable { var botId: String; var seconds: Int }
        let _: Empty = try await call("logCall", Body(botId: botId, seconds: seconds))
    }

    /// A voice call connected: its line goes in the chat now. Throws on a computer older than this method.
    func startCall(_ botId: String) async throws -> String {
        struct Res: Decodable { var entry: Entry }
        let res: Res = try await call("startCall", ["botId": botId])
        return res.entry.id
    }

    /// The call's end: its length goes on the line `entryId` (none: the call never connected). Safe to repeat.
    func endCall(_ botId: String, entryId: String?, seconds: Int) async throws {
        struct Body: Encodable { var botId: String; var entryId: String?; var seconds: Int }
        let _: Empty = try await call("endCall", Body(botId: botId, entryId: entryId, seconds: seconds))
    }

    /// Stops the bot where the user pressed it: its chat, or the thread on `thread`.
    func stop(_ botId: String, thread: String?) async throws {
        var body = ["botId": botId]
        body["threadId"] = thread
        let _: Empty = try await call("stop", body)
    }

    func newSession(_ botId: String) async throws {
        let _: Empty = try await call("newSession", ["botId": botId])
    }

    func respondPermission(entryId: String, optionId: String?) async throws {
        struct Body: Encodable { var entryId: String; var optionId: String? }
        let _: Empty = try await call("respondPermission", Body(entryId: entryId, optionId: optionId))
    }

    /// Push tickets are bound to this device's key; `pushKey` lets the host seal notification text (§6.7).
    /// `labels` says which crypto labels the pairing uses, so the host seals pushes under the same ones;
    /// a host that predates the rename ignores it, and omitting it means the old labels.
    func registerDevice(ticket: String, relay: String, name: String, pushKey: String?, ctx: String,
                        labels: LabelFamily = .legacy) async throws {
        struct Body: Encodable {
            var ticket: String; var relay: String; var name: String; var pushKey: String?; var ctx: String; var labels: String?
        }
        let _: Empty = try await call("registerDevice", Body(ticket: ticket, relay: relay, name: name, pushKey: pushKey, ctx: ctx,
                                                             labels: labels == .kith ? labels.rawValue : nil))
    }

    /// Stops this device's alerts; used when notifications are switched off in the app.
    func unregisterDevice() async throws {
        let _: Empty = try await call("unregisterDevice")
    }

    func registerActivity(botId: String, ticket: String) async throws {
        let _: Empty = try await call("registerActivity", ["botId": botId, "ticket": ticket])
    }

    /// Forget a bot's Live Activity registration once the activity has ended or the phone unpairs.
    func unregisterActivity(botId: String) async throws {
        let _: Empty = try await call("unregisterActivity", ["botId": botId])
    }

    func usage(refresh: Bool = false) async throws -> Usage {
        try await call("usage", ["refresh": refresh])
    }

    func listDirs(_ path: String?, showHidden: Bool = false) async throws -> DirListing {
        struct Body: Encodable { var path: String?; var showHidden: Bool? }
        return try await call("listDirs", Body(path: path, showHidden: showHidden ? true : nil))
    }

    /// A one-time code to send a Kith bot as `/link CODE`, tying that chat account to this person.
    /// An older host answers with `HostError.http(404, …)`; asking again gives a fresh code.
    func linkCode() async throws -> LinkCode {
        try await call("linkCode")
    }

    // MARK: kith bot details (serve/docs/bot-info.md)

    func kithInfo(botId: String, refresh: Bool = false) async throws -> KithInfoResponse {
        struct Body: Encodable { var botId: String; var refresh: Bool? }
        // The host waits up to 45 s for kith.
        return try await call("kithInfo", Body(botId: botId, refresh: refresh ? true : nil), timeout: 60)
    }

    /// Writes an hq bot's permission mode through to hq (`kith <agent> set permission`); the answer is hq's info read back.
    func kithSetPermission(botId: String, mode: String) async throws -> KithInfoResponse {
        struct Body: Encodable { var botId: String; var mode: String }
        return try await call("kithSetPermission", Body(botId: botId, mode: mode), timeout: 60)
    }

    /// Writes an hq bot's call model through to hq (`kith <agent> set call-model`): a model, `own` or `default`.
    func kithSetCallModel(botId: String, model: String) async throws -> KithInfoResponse {
        struct Body: Encodable { var botId: String; var model: String }
        return try await call("kithSetCallModel", Body(botId: botId, model: model), timeout: 60)
    }

    /// The grants ledger: every lasting grant for anyone but the owner, of one agent and/or one person. Owner only: a
    /// member gets 403, an older host 404.
    func accessLedger(agent: String? = nil, person: String? = nil) async throws -> AccessLedger {
        struct Body: Encodable { var agent: String?; var person: String? }
        return try await call("accessLedger", Body(agent: agent, person: person), timeout: 30)
    }

    /// Takes one ledger entry away by its key, so that person's next run no longer gets it.
    func removeGrant(_ grant: AccessGrant) async throws {
        struct Body: Encodable { var kind: String; var key: String }
        struct Removed: Decodable {}
        let _: Removed = try await call("removeGrant", Body(kind: grant.kind, key: grant.key), timeout: 30)
    }

    /// The backup chain (serve/host/src/api/backups.rs); this app only reads it. A host before it answers 404.
    func backups() async throws -> Backups {
        try await call("backups", timeout: 90)
    }

    func kithMemoryFile(botId: String, path: String) async throws -> KithMemoryFile {
        struct Body: Encodable { var botId: String; var path: String }
        return try await call("kithMemoryFile", Body(botId: botId, path: path), timeout: 30)
    }

    func kithMemorySearch(botId: String, query: String, limit: Int = 30) async throws -> KithSearchResult {
        struct Body: Encodable { var botId: String; var query: String; var limit: Int }
        return try await call("kithMemorySearch", Body(botId: botId, query: query, limit: limit), timeout: 30)
    }

    // MARK: place folders

    func projects(refresh: Bool = false) async throws -> ProjectList {
        struct Body: Encodable { var refresh: Bool? }
        return try await call("projects", Body(refresh: refresh ? true : nil), timeout: 30)
    }

    func recentFolders() async throws -> [RecentFolder] {
        struct Res: Decodable { var folders: [RecentFolder] }
        let res: Res = try await call("recentFolders")
        return res.folders
    }

    /// Points a place at a project, a folder, or (`nil`) back at what it inherits. The place starts a fresh session
    /// when the folder it works in changes. `chat`: a group's id, with `botId` the member whose lane it is.
    func setPlaceFolder(botId: String, threadId: String? = nil, chat: String? = nil, to choice: FolderChoice?) async throws -> PlaceFolder {
        struct Body: Encodable {
            var botId: String; var threadId: String?; var chat: String?
            var project: String?; var folder: String?; var clear: Bool?
        }
        struct Res: Decodable { var folder: PlaceFolder }
        let res: Res = try await call("setPlaceFolder", Body(botId: botId, threadId: threadId, chat: chat, project: choice?.projectId,
                                                              folder: choice?.folderPath, clear: choice == nil ? true : nil))
        return res.folder
    }

    func registerProject(id: String, path: String) async throws -> HQProject {
        struct Res: Decodable { var project: HQProject }
        let res: Res = try await call("registerProject", ["id": id, "path": path], timeout: 30)
        return res.project
    }

    // MARK: remote screen

    func screenStatus() async throws -> ScreenState { try await call("screenStatus") }

    func screenPrepare() async throws -> ScreenConnection {
        let relay = if let channel = transport as? ChannelTransport { try await channel.screenRelayRequired() } else { false }
        return try await call("screenPrepare", ["relay": relay])
    }

    /// Sends a WebRTC offer (with all its ICE candidates); returns the session id and the answer.
    /// Passing `session` restarts ICE on an existing session.
    func screenOffer(sdp: String, session: String?, display: UInt32?) async throws -> ScreenAnswer {
        struct Body: Encodable { var sdp: String; var session: String?; var display: UInt32? }
        return try await call("screenOffer", Body(sdp: sdp, session: session, display: display), timeout: 30)
    }

    func screenClose(session: String) async throws {
        let _: Empty = try await call("screenClose", ["session": session])
    }

    /// Takes control from bots (they can still look) or hands it back.
    func screenTakeover(_ on: Bool) async throws -> ScreenState {
        try await call("screenTakeover", ["on": on])
    }

}

// MARK: - Agent setup

public extension HostClient {
    func refreshBackends() async throws -> [Backend] {
        struct Res: Decodable { var backends: [Backend] }
        let res: Res = try await call("refreshBackends", timeout: 60)
        return res.backends
    }

    /// Starts (or rejoins) installing or signing in to `backend` in a terminal on the computer.
    /// `method`: a terminal sign-in method from `agentAuth`; nil runs Kith's own command.
    func agentSetup(backend: String, step: SetupStep, method: String?, cols: Int, rows: Int) async throws -> String {
        struct Body: Encodable { var backend: String; var step: SetupStep; var method: String?; var cols: Int; var rows: Int }
        struct Res: Decodable { var term: String }
        // The first sign-in through a registry build can download it.
        let res: Res = try await call("agentSetup", Body(backend: backend, step: step, method: method, cols: cols, rows: rows), timeout: 300)
        return res.term
    }

    /// Starts the agent to see whether it's signed in and how it can be (first run may download it).
    func agentModels(_ backend: String) async throws -> AgentModels {
        try await call("agentModels", ["backend": backend], timeout: 300)
    }

    func agentAuth(_ backend: String) async throws -> AgentAuth {
        try await call("agentAuth", ["backend": backend], timeout: 300)
    }

    /// Lets the agent sign itself in; returns once it's done (or gave up).
    func agentAuthenticate(_ backend: String, method: String) async throws -> AgentAuth {
        try await call("agentAuthenticate", ["backend": backend, "method": method], timeout: 660)
    }

    /// Saves keys for the agent on the computer (empty values remove them).
    func setAgentEnv(_ backend: String, vars: [String: String]) async throws -> AgentAuth {
        struct Body: Encodable { var backend: String; var vars: [String: String] }
        return try await call("setAgentEnv", Body(backend: backend, vars: vars), timeout: 300)
    }

    func termInput(_ term: String, _ bytes: Data) async throws {
        let _: Empty = try await call("termInput", ["term": term, "data": bytes.base64EncodedString()])
    }

    func termResize(_ term: String, cols: Int, rows: Int) async throws {
        struct Body: Encodable { var term: String; var cols: Int; var rows: Int }
        let _: Empty = try await call("termResize", Body(term: term, cols: cols, rows: rows))
    }

    func termClose(_ term: String) async throws {
        let _: Empty = try await call("termClose", ["term": term])
    }
}

/// What the host's `linkCode` returns; `expiresAt` is epoch milliseconds.
public struct LinkCode: Decodable, Sendable, Equatable {
    public var code: String
    public var expiresAt: Double

    public var expiry: Date { Date(timeIntervalSince1970: expiresAt / 1000) }
    public init(code: String, expiresAt: Double) { self.code = code; self.expiresAt = expiresAt }
}

public struct ScreenAnswer: Codable, Sendable {
    public var session: String
    public var sdp: String
}
