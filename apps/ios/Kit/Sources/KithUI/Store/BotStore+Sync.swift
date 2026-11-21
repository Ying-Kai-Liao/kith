import KithKit
import Foundation
import os

private let log = Logger(subsystem: "com.yingkailiao.kith", category: "BotStore")

extension BotStore {
    /// Events (catch-up since `rev`, then live) for as long as the link stays ready.
    func runEvents(_ client: HostClient) async {
        var backoff: Double = 1
        while !Task.isCancelled {
            do {
                // `rev` is the cache's until it is read.
                await cacheReady()
                // What the last stream sent goes in before the hello (which may rewind) and before `rev` is read.
                flushEvents()
                try await proveInviteIfNeeded(client)
                try await refreshHello(client)
                guard !Task.isCancelled, !retired else { return }
                if mismatch != nil {
                    // Reached, but nothing to sync until one side updates. A host update restarts
                    // the link (and this loop) anyway; this catches one that doesn't.
                    setConnection(.online)
                    try await Task.sleep(for: Self.mismatchRecheck)
                    continue
                }
                requestedSince = rev
                windowFloors = [:]
                for try await event in client.events(since: requestedSince, client: "ios") {
                    guard !Task.isCancelled, !retired else { return }
                    setConnection(.online)
                    backoff = 1
                    enqueue(event)
                }
            } catch is CancellationError {
                return
            } catch {
                if Task.isCancelled || retired { return }
                log.info("stream ended: \(error.localizedDescription)")
                if case let HostError.unauthorized(message) = error {
                    setConnection(.unauthorized(message))
                    return
                }
            }
            try? await Task.sleep(for: .seconds(backoff))
            backoff = min(backoff * 2, 20)
        }
    }

    /// An invited person's device shows the invite token first; until it does the computer refuses everything else,
    /// the event stream included. The token stays in the Keychain, so a reconnect or a new pairing can do it again.
    func proveInviteIfNeeded(_ client: HostClient) async throws {
        guard let invite = inviteTokens?.stored(for: computer.id) else { return }
        do {
            try await client.proveInvite(inviteId: invite.inviteId, token: invite.token)
        } catch {
            // A device that already showed it is the normal case on every later connection.
            if InviteProof.isAlreadyProven(error) { return }
            // A computer that has no such call can't want the token.
            if InviteProof.isUnknownMethod(error) { return }
            // Refused for good, unless it is only the try limit: that passes with time.
            if case let HostError.http(_, message) = error, !message.localizedCaseInsensitiveContains("too many") {
                throw HostError.unauthorized(message)
            }
            throw error
        }
    }

    /// An invited person's own chat with a granted agent: the host makes it the first time and returns it after.
    /// Its answer ("Chatting with agents isn't open to guests and members yet.") comes back as the thrown error.
    public func startChat(agent: String) async throws -> Bot {
        guard let client else { throw HostError.unreachable }
        let bot = try await client.startChat(agent: agent)
        if !retired {
            apply(.bot(bot))
            flushAcks()
            scheduleSave()
        }
        return bot
    }

    /// Asked on every (re)connect: the host may have been updated while this app was away.
    private func refreshHello(_ client: HostClient) async throws {
        let h: Hello
        do {
            h = try await client.hello()
        } catch let error as DecodingError {
            // Its version and protocol levels still say which side to update.
            let version: HostVersion = try await client.call("hello")
            guard !Task.isCancelled, !retired else { return }
            log.error("undecodable hello from host \(version.version)")
            noteHostVersion(version)
            Motion.animate { unreadable = true }
            if mismatch == nil { throw error }
            return
        }
        guard !Task.isCancelled, !retired else { return }
        // hello first: whatever the version change shows (notices, reminders) reads it.
        hello = h
        // Pending bots come back from the host on every connect, so a card survives a restart.
        Task { [weak self] in await self?.botGuests.refresh() }
        Task { [weak self] in await self?.refreshMyRooms() }
        noteHostVersion(HostVersion(version: h.version, protocolLevel: h.protocolLevel, minProtocol: h.minProtocol))
    }

    private func noteHostVersion(_ new: HostVersion) {
        let cached = cacheStamp.map { $0 != "\(Self.appBuild)/\(new.version)\(Self.cacheSchema)" } ?? false
        if cached || (hostVersion.map { $0.version != new.version } ?? false) {
            // A different host version or app build: data may carry new fields, so fetch it all again.
            refetchAll()
            rewound = false
            unreadable = false
        }
        cacheStamp = nil
        // An update notice may appear or go away with it.
        if new != hostVersion { Motion.animate { hostVersion = new } }
    }

    /// Holds an event for the next batch. What arrives within the wait (a catch-up is thousands of events) is applied in
    /// one go: the chats and the roster redraw once per batch, not once per event, and a chat on screen is
    /// acknowledged once.
    private func enqueue(_ event: HostEvent) {
        pendingEvents.append(event)
        guard flushTask == nil else { return }
        let wait = caughtUp ? Self.liveBatchWait : Self.catchUpBatchWait
        flushTask = Task { [weak self] in
            try? await Task.sleep(for: wait)
            guard !Task.isCancelled else { return }
            self?.flushEvents()
        }
    }

    /// Applies the events held so far, in order. A restart (`resync`, a rewind) ends the batch: the rest came on the
    /// stream it replaces, as the old per-event loop stopped reading it.
    func flushEvents() {
        flushTask?.cancel()
        flushTask = nil
        guard !pendingEvents.isEmpty, !retired else { return }
        let batch = pendingEvents
        pendingEvents = []
        let generation = eventsGeneration
        for event in batch {
            apply(event)
            if eventsGeneration != generation { break }
        }
        flushAcks()
        scheduleSave()
    }

    private func apply(_ event: HostEvent) {
        switch event {
        case let .hello(id, hostRev, newUsage, newScreen):
            if let hostId, hostId != id {
                // Different host database: start over.
                resetMirror()
            }
            hostId = id
            // The host repeats the calls still ringing right after a hello.
            incomingCalls = []
            catchUpTask?.cancel()
            catchUpTask = Task { [weak self] in
                try? await Task.sleep(for: Self.catchUpWait)
                guard !Task.isCancelled, let self else { return }
                self.onCallCatchUp?(Set(self.incomingCalls.map(\.id)))
            }
            setUsage(newUsage)
            screen = newScreen
            let hostRewound = hostRev < rev
            if hostRewound { rev = 0 }
            windowFloors = requestedSince > 0 && !hostRewound ? loadedFloors() : [:]
            catchUpRev = hostRev
            // Each connection's catch-up batches as the first one does (`enqueue`); `bump` sets it again at once
            // when nothing is owed.
            caughtUp = false
            bump(rev)
            // Live changes come as events; what was already waiting is read once per connection.
            Task { [weak self] in await self?.refreshAccessRequests() }
        case let .bot(bot):
            guard closedRooms[bot.id] == nil else { break }
            let neededInput = bots[bot.id]?.needsInput == true
            let before = bots[bot.id]
            bots[bot.id] = bot
            bump(bot.rev)
            // A folder change from another client moves no status: re-read the threads whose folder is shown.
            if before?.status == bot.status, before?.workingThread == bot.workingThread { refreshThreadFolders(bot.id) }
            onBotUpdated?(bot)
            onRosterChanged?()
            if bot.unread > 0 { noteVisibleConversations(bot.id) }
            if let before, before.isWorking != bot.isWorking {
                for call in Array(voiceCalls.values) where call.botId == bot.id { call.working?(bot.isWorking) }
            }
            if bot.needsInput && !neededInput {
                for call in Array(voiceCalls.values) where call.botId == bot.id {
                    call.announce("\(bot.name) needs your approval in the chat.")
                }
            }
        case let .botDeleted(id, r):
            removeComposerDrafts(for: id)
            bots[id] = nil
            entries[id] = nil
            windowFloors[id] = nil
            bump(r)
            if selection == id { selection = nil }
            onRosterChanged?()
        case let .entry(e):
            if Self.isOutsideLoadedWindow(e, floor: windowFloors[e.botId], held: entries[e.botId]?.contains { $0.id == e.id } == true) {
                bump(e.rev)
                break
            }
            let known = entries[e.botId]?.first(where: { $0.id == e.id })
            upsert(e)
            bump(e.rev)
            noteVisibleConversations(e.botId, entry: e)
            let calls = Array(voiceCalls.values).filter { $0.botId == e.botId && e.rev > $0.startRev }
            guard !calls.isEmpty else { break }
            // The chat's replies and its tasks' are heard; other threads aren't.
            let heard = e.threadId.map { root in chat(e.botId).contains { $0.id == root && $0.isTask } } ?? true
            // A shared file's entry carries only its name and size for the chat's card: never read aloud.
            if e.kind == "agent", heard, known?.data.final != true, e.data.files?.isEmpty != false, let text = e.data.text, !text.isEmpty {
                let final = e.data.final == true
                for call in calls {
                    // Replies already final when the call began never get here (`known` was final).
                    if let stream = call.stream { stream(e.id, spokenReply(e, text), final) }
                    else if final { call.speak(spokenReply(e, text)) }
                }
            }
            // A group has no status of its own: its members' approvals arrive as cards.
            if e.kind == "permission", known == nil, e.data.status == "pending", bots[e.botId]?.isGroup == true {
                let name = e.data.author.flatMap { bots[$0]?.name } ?? "A bot"
                calls.forEach { $0.announce("\(name) needs your approval in the chat.") }
            }
        case let .usage(u):
            setUsage(u)
        case let .screen(s):
            screen = s
        case let .call(event):
            onCallEvent?(event)
            if event.ringing && CallPresence.active > 0 {
                declineCall(event.call)
            } else {
                incomingCalls = IncomingCalls.apply(incomingCalls, event)
            }
        case .resync:
            restartEvents()
        case .botRequest, .botUpdated:
            Task { [weak self] in await self?.botGuests.refresh() }
        case let .accessRequests(requests):
            accessRequests = requests
        case let .conversation(conversation):
            roomConversations.note(conversation)
        case let .undecodable(type):
            // Never skip past data we couldn't read: rewind once and fetch everything again.
            log.error("undecodable \(type) event")
            if !rewound {
                rewound = true
                refetchAll()
                restartEvents()
            } else if !unreadable {
                Motion.animate { unreadable = true }
                // A newer host this app can't follow: stop syncing and ask for an update.
                if mismatch != nil { restartEvents() }
            }
        }
    }

    /// In a group, a reply is said with its speaker's name.
    private func spokenReply(_ e: Entry, _ text: String) -> String {
        guard bots[e.botId]?.isGroup == true, let name = e.data.author.flatMap({ bots[$0]?.name }) else { return text }
        return "\(name): \(text)"
    }

    func resetMirror() {
        // Events not applied yet would put back what is wiped here (an invited person turned away). Inside a flush
        // the batch is already taken, so nothing of the new mirror is lost.
        flushTask?.cancel()
        flushTask = nil
        pendingEvents = []
        pendingAcks = []
        composerDrafts = [:]
        persistComposerDrafts()
        bots = [:]
        entries.transform { $0.filter { $0.id.hasPrefix("local-") } }
        selection = nil
        onRosterChanged?()
        rev = 0
        hostId = nil
        historyComplete = []
        roomConversations = RoomConversations()
        windowFloors = [:]
        screen = nil
        incomingCalls = []
        saveCache()
    }

    /// Fetches everything again from rev 0. That catch-up brings only each chat's newest entries, so
    /// the main-chat ones held now go too: an older one kept would hide the gap from `loadOlder`, which
    /// pages from the oldest one held. Threads (loaded whole when opened) and messages still sending stay.
    private func refetchAll() {
        rev = 0
        entries.transform { $0.filter { $0.threadId != nil || $0.id.hasPrefix("local-") } }
        historyComplete = []
    }

    /// Resubscribes from the current `rev` on the same link.
    private func restartEvents() {
        // Before the guard: a batch applied while the link is down must stop at this event too.
        eventsGeneration += 1
        guard let client, eventsTask != nil else { return }
        eventsTask?.cancel()
        eventsTask = Task { [weak self] in await self?.runEvents(client) }
    }

    private func bump(_ r: Int64) {
        rev = max(rev, r)
        if !caughtUp && rev >= catchUpRev { caughtUp = true }
    }

    /// Per bot, the lowest synced main-chat `seq` in the mirror (optimistic entries don't count).
    /// Only entries the mirror had at `requestedSince`: newer ones (a send's RPC response landing
    /// before this hello) come again in this catch-up and say nothing about the window.
    private func loadedFloors() -> [String: Int64] {
        // An old thread root held for its fresh replies says nothing about the window (`oldestWindowSeq`).
        entries.all.compactMapValues { list in Self.oldestWindowSeq(list, upTo: requestedSince) }
    }

    /// An entry event the mirror doesn't hold, in the main chat, below the floor the mirror had
    /// when this connection started: it existed before the window and arrives only because its
    /// rev was bumped (a rewrite, a reaction). Inserting it would hide the gap from `loadOlder`,
    /// which pages from the oldest loaded `seq`; history paging brings it. `seq` follows insert
    /// order, so anything created since the mirror was last synced sits above the floor.
    static func isOutsideLoadedWindow(_ e: Entry, floor: Int64?, held: Bool) -> Bool {
        guard e.threadId == nil, !held, let floor else { return false }
        return e.seq < floor
    }

    func upsert(_ e: Entry) {
        // A room the person left or was removed from is gone: nothing more of it is kept.
        guard closedRooms[e.botId] == nil else { return }
        // In place: copying the chat for every event made a catch-up quadratic.
        Self.upsert(e, into: &entries[e.botId, default: []])
    }

    static func upsert(_ e: Entry, into list: inout [Entry]) {
        if let i = list.firstIndex(where: { $0.id == e.id }) {
            // A late API response mustn't undo a newer SSE update.
            guard e.rev >= list[i].rev else { return }
            list[i] = e
        } else {
            // Replace the optimistic copy of a message we sent.
            if e.kind == "user", let nonce = e.data.clientNonce, !nonce.isEmpty {
                list.removeAll { $0.id == "local-\(nonce)" }
            }
            if let last = list.last, last.seq > e.seq, e.seq > 0 {
                let i = list.firstIndex { $0.seq > e.seq } ?? list.endIndex
                list.insert(e, at: i)
            } else {
                list.append(e)
            }
        }
    }

    func setUsage(_ u: Usage) {
        guard u != usage, !retired else { return }
        usage = u
        storage.usage[computer.id] = u
        onUsageChanged?(computer.id, u)
    }
}
