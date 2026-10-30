import KithKit
import Foundation
import os

private let log = Logger(subsystem: "com.yingkailiao.kith", category: "BotStore")

extension BotStore {
    private func composerKey(_ botId: String, _ thread: String?) -> String {
        // Encode the tuple so a main chat cannot collide with a thread's ID.
        String(decoding: (try? JSONEncoder().encode([botId, thread])) ?? Data(), as: UTF8.self)
    }

    func composerDraft(for botId: String, thread: String? = nil) -> String {
        composerDrafts[composerKey(botId, thread)] ?? ""
    }

    func setComposerDraft(_ text: String, for botId: String, thread: String? = nil) {
        guard !retired else { return }
        let key = composerKey(botId, thread)
        if text.isEmpty {
            composerPasted[key] = nil
        } else if let pasted = pastedInsertion(from: composerDrafts[key] ?? "", to: text) {
            composerPasted[key, default: []].append(pasted)
        }
        composerDrafts[key] = text.isEmpty ? nil : text
        persistComposerDrafts()
    }

    func persistComposerDrafts() {
        storage.composerDrafts[computer.id] = composerDrafts.isEmpty ? nil : composerDrafts
    }

    func removeComposerDrafts(for botId: String) {
        composerDrafts = composerDrafts.filter { key, _ in
            let destination = try? JSONDecoder().decode([String?].self, from: Data(key.utf8))
            return destination?.first != botId
        }
        persistComposerDrafts()
    }

    /// Transfer to the outbox now; an eventual acknowledgement must not erase a newer draft.
    @discardableResult
    func sendComposerDraft(to botId: String, thread: String? = nil, files: [OutgoingFile] = [], newTask: Bool = false,
                           start: FolderChoice? = nil, budget: Int? = nil) -> Bool {
        let typed = composerDraft(for: botId, thread: thread)
        guard !retired, !typed.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !files.isEmpty else { return false }
        // A reply quotes its message; with nothing typed (files only) there is nothing to attach it to.
        let quote = thread == nil ? replyQuotes[botId].flatMap(ReplyQuote.line(of:)) : nil
        let text = typed.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? typed : ReplyQuote.compose(quote: quote, text: typed)
        let pasted = composerPasted[composerKey(botId, thread)] ?? []
        send(text, to: botId, thread: thread, nonce: UUID().uuidString, files: files, pasted: pasted, newTask: newTask,
             start: start, budget: budget)
        replyQuotes[botId] = nil
        setComposerDraft("", for: botId, thread: thread)
        return true
    }

    /// `thread`: reply in the thread on that main-chat message.
    /// `newTask`: starts a new task (a fresh session) instead of going on with the current one, as the composer's
    /// "new task" does.
    /// `budget`: to a group, the turns this message gives the room (1...100); a chat with one agent ignores it.
    public func send(_ text: String, to botId: String, thread: String? = nil, files: [OutgoingFile] = [], newTask: Bool = false,
                     budget: Int? = nil) {
        send(text, to: botId, thread: thread, nonce: UUID().uuidString, files: files, newTask: newTask, budget: budget)
    }

    /// A message spoken in a voice call: the agent answers it for the ear, on its call model (`via: "call"`).
    public func sendSpoken(_ text: String, to botId: String) {
        send(text, to: botId, thread: nil, nonce: UUID().uuidString, via: "call")
    }

    private func send(_ text: String, to botId: String, thread: String?, nonce: String, files: [OutgoingFile] = [],
                      pasted: [String] = [], newTask: Bool = false, start: FolderChoice? = nil, via: String? = nil,
                      budget: Int? = nil) {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty || !files.isEmpty, !retired else { return }
        var data = EntryData(text: trimmed, status: "sending", clientNonce: nonce)
        if !files.isEmpty {
            data.attachments = files.map { Attachment(id: $0.id.uuidString, name: $0.name, size: Int64($0.data.count)) }
            outgoingFiles[nonce] = files
        }
        let local = Entry(
            id: "local-\(nonce)", seq: Int64.max, botId: botId, threadId: thread, rev: 0, kind: "user",
            turn: 0, data: data, createdAt: Int64(Date.now.timeIntervalSince1970 * 1000), updatedAt: 0
        )
        upsert(local)
        storage.lastComputerId = computer.id
        if let bot = bots[botId] { onSent?(bot, .sending) }
        if let start { outgoingStarts[nonce] = start }
        if newTask { outgoingNewTasks.insert(nonce) }
        // Only a group has turns. The standard ten is the host's own default, so it never needs to travel.
        let turns = bots[botId]?.isGroup == true ? budget.map(Turns.clamp) : nil
        if let turns { outgoingBudgets[nonce] = turns }
        // The relay mailbox holds text only: files, a starting folder and a chosen number of turns wait for the computer.
        if canQueue, files.isEmpty, start == nil, (turns ?? Turns.standard) == Turns.standard,
           let remote = transport as? any RemoteTransport {
            enqueue(remote, text: trimmed, botId: botId, thread: thread, nonce: nonce)
            return
        }
        deliver(trimmed, botId: botId, thread: thread, nonce: nonce, pasted: pasted, newTask: newTask, start: start, via: via,
                budget: turns)
    }

    private func deliver(_ text: String, botId: String, thread: String?, nonce: String, pasted: [String] = [],
                         newTask: Bool = false, start: FolderChoice? = nil, via: String? = nil, budget: Int? = nil) {
        Task {
            do {
                // Safe to repeat: the computer skips a nonce it already has.
                let e = try await withLink(replay: true) { client in
                    var ids: [String]?
                    if let files = self.outgoingFiles[nonce] {
                        // A fresh upload per attempt: a retry never appends to a half-sent file.
                        ids = []
                        for file in files {
                            let id = UUID().uuidString
                            try await client.upload(botId: botId, id: id, name: file.name, data: file.data)
                            Self.cacheAttachment(id, file.data)
                            ids?.append(id)
                        }
                    }
                    return try await client.send(botId: botId, text: text, clientNonce: nonce, threadId: thread, attachments: ids,
                                                 pasted: pasted, newTask: newTask, start: start, via: via, budget: budget)
                }
                outgoingFiles[nonce] = nil
                outgoingStarts[nonce] = nil
                outgoingBudgets[nonce] = nil
                outgoingNewTasks.remove(nonce)
                upsert(e)
                if let bot = bots[botId] { onSent?(bot, .delivered) }
            } catch {
                if outgoingFiles[nonce] != nil { lastError = "Couldn't send the files: \(error.localizedDescription)" }
                markLocal(nonce: nonce, botId: botId, status: "failed")
            }
        }
    }

    /// The computer is offline: the message waits, sealed for it, in the relay mailbox.
    private func enqueue(_ remote: any RemoteTransport, text: String, botId: String, thread: String?, nonce: String) {
        markLocal(nonce: nonce, botId: botId, status: "waiting")
        saveCache()
        Task {
            do {
                try await remote.enqueue(botId: botId, text: text, clientNonce: nonce, threadId: thread)
                if let bot = bots[botId] { onSent?(bot, .queued) }
            } catch MailboxError.hostOnline {
                // Presence can race the stream state. Wait for the host's events channel
                // before trying the normal API, instead of turning that race into a failure.
                markLocal(nonce: nonce, botId: botId, status: "sending")
                await deliverWhenOnline(text, botId: botId, thread: thread, nonce: nonce)
            } catch {
                // A dropped relay link: the message's own "Failed to send" and Resend say enough.
                if (error as? HostError)?.isTransient != true { lastError = error.localizedDescription }
                markLocal(nonce: nonce, botId: botId, status: "failed")
            }
            saveCache()
        }
    }

    private func deliverWhenOnline(_ text: String, botId: String, thread: String?, nonce: String) async {
        let deadline = ContinuousClock.now + .seconds(30)
        while !retired, connection != .online, ContinuousClock.now < deadline {
            try? await Task.sleep(for: .milliseconds(200))
        }
        guard !retired else { return }
        guard connection == .online else {
            lastError = "The computer came online, but the message couldn't be sent. Retry it when the connection is ready."
            markLocal(nonce: nonce, botId: botId, status: "failed")
            saveCache()
            return
        }
        deliver(text, botId: botId, thread: thread, nonce: nonce)
    }

    /// Takes a waiting message back out of the mailbox, unless the computer already has it.
    public func cancelQueued(_ entry: Entry) {
        guard let nonce = entry.data.clientNonce, let remote = transport as? any RemoteTransport else { return }
        Task {
            switch await remote.cancelQueued(clientNonce: nonce) {
            case .cancelled:
                discard(entry)
                if let bot = bots[entry.botId] { onSent?(bot, .cancelled) }
            case .delivering:
                markLocal(nonce: nonce, botId: entry.botId, status: "delivering")
            case .unknown:
                lastError = "Couldn't take the message back. It may have been delivered already."
            }
            saveCache()
        }
    }

    func applyMailbox(_ event: MailboxEvent) {
        guard let entry = entries.values.lazy.flatMap({ $0 }).first(where: { $0.id == "local-\(event.nonce)" }) else { return }
        switch event {
        // The events stream's own copy replaces it (matched by clientNonce).
        case .delivered: markLocal(nonce: event.nonce, botId: entry.botId, status: "delivering")
        case .failed, .expired: markLocal(nonce: event.nonce, botId: entry.botId, status: "failed")
        }
        saveCache()
    }

    /// Messages the relay no longer holds were delivered, refused or expired while we were away.
    /// "Failed" is safe even if one was delivered: the events catch-up replaces it by clientNonce,
    /// and a resend reuses that nonce, so the computer never runs it twice.
    func reconcileQueued(_ remote: any RemoteTransport) async {
        let waiting = entries.values.flatMap { $0 }.filter { $0.data.status == "waiting" }
        guard !waiting.isEmpty else { return }
        let heldItems: [QueuedItem]
        do {
            heldItems = try await remote.listQueued()
        } catch {
            log.debug("mailbox reconciliation deferred: \(error.localizedDescription)")
            return
        }
        let held = Dictionary(heldItems.map { ($0.nonce, $0.state) }, uniquingKeysWith: { a, _ in a })
        guard !retired else { return }
        for e in waiting {
            guard let nonce = e.data.clientNonce else { continue }
            switch held[nonce] {
            case "queued": break
            case .some: markLocal(nonce: nonce, botId: e.botId, status: "delivering")
            case nil: markLocal(nonce: nonce, botId: e.botId, status: "failed")
            }
        }
        saveCache()
    }

    public func retry(_ entry: Entry) {
        let files = entry.data.clientNonce.flatMap { outgoingFiles[$0] } ?? []
        guard let text = entry.data.text, !text.isEmpty || !files.isEmpty else { return }
        entries[entry.botId]?.removeAll { $0.id == entry.id }
        // The same clientNonce: if the computer got it after all, it won't run twice (the host
        // skips a nonce it has). Every seal still takes a fresh ephemeral key (§6.4).
        send(text, to: entry.botId, thread: entry.threadId, nonce: entry.data.clientNonce ?? UUID().uuidString, files: files,
             newTask: entry.data.clientNonce.map { outgoingNewTasks.contains($0) } ?? false,
             start: entry.data.clientNonce.flatMap { outgoingStarts[$0] },
             budget: entry.data.clientNonce.flatMap { outgoingBudgets[$0] })
    }

    /// A sent file's bytes: on disk after the first fetch (sent files never change).
    public func attachmentData(_ file: Attachment, bot botId: String) async -> Data? {
        guard let url = Self.attachmentCache(file.id) else { return nil }
        if let data = try? Data(contentsOf: url) { return data }
        guard let client = live, let data = try? await client.readUpload(botId: botId, id: file.id) else { return nil }
        Self.cacheAttachment(file.id, data)
        return data
    }

    private static func attachmentCache(_ id: String) -> URL? {
        guard let uuid = UUID(uuidString: id) else { return nil }
        return URL.cachesDirectory.appending(path: "kith-attachments").appending(path: uuid.uuidString)
    }

    private static func cacheAttachment(_ id: String, _ data: Data) {
        guard let url = attachmentCache(id) else { return }
        try? FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try? data.write(to: url)
    }

    public func discard(_ entry: Entry) {
        if let nonce = entry.data.clientNonce { outgoingFiles[nonce] = nil; outgoingBudgets[nonce] = nil }
        entries[entry.botId]?.removeAll { $0.id == entry.id }
        scheduleSave()
    }

    private func markLocal(nonce: String, botId: String, status: String) {
        guard var list = entries[botId], let i = list.firstIndex(where: { $0.id == "local-\(nonce)" }) else { return }
        list[i].data.status = status
        entries[botId] = list
        scheduleSave()
        if status == "failed", let bot = bots[botId] { onSent?(bot, .failed) }
    }
}
