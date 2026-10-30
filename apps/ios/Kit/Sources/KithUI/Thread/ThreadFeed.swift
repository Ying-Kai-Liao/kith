import KithKit
import SwiftUI

/// Pure mapping from the store's entries onto the redesign's components: the thread chip, tool rows and the order
/// of an open thread. No views here, so it is tested.
enum ThreadFeed {
    /// An opened run of steps shows its newest this many; older ones sit under "N earlier steps" (a long task can run hundreds).
    static let visibleSteps = 20

    // MARK: place

    static func place(_ folder: PlaceFolder?) -> ThreadChipModel.Place? {
        guard let folder, !folder.name.isEmpty else { return nil }
        return folder.kind == "project" ? .project(folder.name) : .folder(folder.name)
    }

    // MARK: chip

    /// What hangs under a task's first message. `yourTurn`: a question waits for the reader.
    static func chip(face: CrewFace, tint: AgentTint, count: Int, unread: Int?, lastAt: Int64, place: PlaceFolder?,
                     yourTurn: Bool, summary: ThreadSummary? = nil, mac: String? = nil, now: Date = .now) -> ThreadChipModel {
        ThreadChipModel(face: face, replies: count, newReplies: max(0, unread ?? 0),
                        time: RelativeTime.day(Date(milliseconds: lastAt), now: now), place: self.place(place),
                        yourTurn: yourTurn, tint: tint, origin: origin(summary), originLabel: summary?.runLabel, writer: writerChip(summary, mac: mac))
    }

    // MARK: terminal threads (continue anywhere)

    /// The origin tag for a thread started outside the app; nil for the app, a bot, or a word we don't know.
    static func origin(_ summary: ThreadSummary?) -> OriginTag.Origin? {
        switch summary?.threadOrigin {
        case .terminal: .terminal
        case .discord: .discord
        case .telegram: .telegram
        case .slack: .slack
        default: nil
        }
    }

    /// The chip while somebody else holds the thread: "In terminal · Mac mini", or working.
    static func writerChip(_ summary: ThreadSummary?, mac: String?) -> StatusChip.Kind? {
        guard let summary, summary.isHeldElsewhere else { return nil }
        return .writer(working: summary.liveBusy == true, mac: mac)
    }

    /// What the dock's place shows for a held thread; nil when the app may write.
    /// `unreachable`: the computer is offline, so what the terminal is doing is unknown.
    static func writerPhase(_ summary: ThreadSummary?, taking: Bool, failed: Bool, unreachable: Bool) -> WriterPhase? {
        guard let summary, summary.isHeldElsewhere else { return nil }
        if taking { return .taking }
        if unreachable { return .unreachable }
        if failed { return .failed }
        return summary.liveBusy == true ? .working : .idle
    }

    /// The divider words of a writer-change notice; nil for any other notice (including a change we don't know).
    static func writerDivider(_ entry: Entry, mac: String) -> WriterDivider.Kind? {
        switch entry.data.writerChangeKind {
        case .startedInTerminal: .startedInTerminal(mac: mac)
        case .openedInTerminal: .openedInTerminal(mac: mac)
        case .tookOver: .tookOver
        case .terminalClosed: .terminalClosed
        case .other, nil: nil
        }
    }

    // MARK: tool rows

    private static let verbs = [
        "read": "Read", "edit": "Edited", "delete": "Deleted", "move": "Moved", "search": "Searched",
        "execute": "Ran", "think": "Thought", "fetch": "Fetched",
    ]

    /// Words a host title may open with per kind ("Read src/pace.js"); the row already has the verb.
    private static let titleVerbs: [String: Set<String>] = [
        "read": ["read", "reading"],
        "edit": ["edit", "edited", "editing", "write", "wrote", "writing", "written"],
        "delete": ["delete", "deleted", "deleting", "remove", "removed", "removing"],
        "move": ["move", "moved", "moving", "rename", "renamed", "renaming"],
        "search": ["search", "searched", "searching"],
        "think": ["think", "thought", "thinking"],
        "fetch": ["fetch", "fetched", "fetching"],
    ]

    /// "Read src/pace.js" -> "src/pace.js" for a read row; a title that is only the verb stays as it is.
    static func withoutVerb(_ title: String, kind: String?) -> String {
        guard let kind, let words = titleVerbs[kind], let space = title.firstIndex(of: " ") else { return title }
        let rest = title[title.index(after: space)...].drop { $0 == " " }
        guard !rest.isEmpty, words.contains(title[..<space].lowercased()) else { return title }
        return String(rest)
    }

    /// `mcp__<server>__<tool>` out of a tool entry's title or command; the entry has no tool-name field of its own.
    static func mcpName(_ entry: Entry) -> (server: String, tool: String)? {
        for text in [entry.data.title, entry.data.command] {
            guard let text, let start = text.range(of: "mcp__") else { continue }
            let word = text[start.upperBound...].prefix { !$0.isWhitespace && $0 != "(" && $0 != "\"" && $0 != "'" }
            guard let sep = word.range(of: "__"), sep.lowerBound > word.startIndex, sep.upperBound < word.endIndex else { continue }
            return (String(word[..<sep.lowerBound]), String(word[sep.upperBound...]))
        }
        return nil
    }

    /// A tool whose result is the item it delivered to the chat (a file or image), so the step itself is noise.
    static func isDelivery(_ entry: Entry) -> Bool {
        guard let name = mcpName(entry) else { return false }
        return name.server == "kith-files" && name.tool == "send_file"
    }

    /// "send_file" -> "Send file".
    static func humanTool(_ tool: String) -> String {
        let words = tool.map { $0 == "_" || $0 == "-" ? " " : $0 }
        let text = String(words).split(separator: " ").joined(separator: " ")
        return text.prefix(1).uppercased() + text.dropFirst()
    }

    private static let genericVerbs: Set<String> = ["used", "using", "use", "called", "calling", "call", "ran", "running"]

    /// One tool entry as one row: "Edited kith-sessions.py  +6 -2". The sheet gets the command and what it printed.
    static func toolRow(_ entry: Entry, patches withPatches: Bool = true) -> ToolCallRowModel {
        let d = entry.data
        let mcp = mcpName(entry)
        let verb = mcp.map { humanTool($0.tool) } ?? verbs[d.toolKind ?? ""] ?? "Used"
        let diffs = d.diffs ?? []
        let object: String
        if let first = diffs.first {
            object = (first.path as NSString).lastPathComponent + (diffs.count > 1 ? " +\(diffs.count - 1)" : "")
        } else {
            let line = (d.title ?? "").split(whereSeparator: \.isNewline).first.map(String.init) ?? ""
            if mcp != nil {
                // The verb is the tool; what is left of the title (without the raw name or "Used") is the object.
                var words = line.split(separator: " ").filter { !$0.contains("mcp__") }
                if let first = words.first, genericVerbs.contains(first.lowercased()) { words.removeFirst() }
                object = words.joined(separator: " ")
            } else {
                object = withoutVerb(line, kind: d.toolKind)
            }
        }
        let result: ToolCallRowModel.Result? = switch d.status {
        case "completed": .text(diffs.isEmpty ? "done" : "+\(diffs.map(\.added).reduce(0, +)) -\(diffs.map(\.removed).reduce(0, +))")
        case "failed": .error("failed")
        default: nil
        }
        let command = d.command?.nilIfEmpty ?? (d.toolKind == "execute" ? d.title?.nilIfEmpty : nil)
        let patches = withPatches ? diffs.map(\.patch).joined(separator: "\n") : ""
        let output = [d.output?.nilIfEmpty, patches.nilIfEmpty].compactMap { $0 }.joined(separator: "\n\n").nilIfEmpty
        return ToolCallRowModel(verb: verb, object: object, result: result, command: command, output: output)
    }

    // MARK: open thread

    /// What an open thread shows under its first message, in order.
    enum Reply: Identifiable, Equatable {
        case entry(Entry, groupStart: Bool)
        /// A run of tool calls behind one collapsed "N steps" row.
        case steps(StepsRun)
        /// A gap of more than an hour (thread screen only).
        case separator(Date, id: String)

        var id: String {
            switch self {
            case .entry(let e, _): ThreadFeed.stableId(e)
            case .steps(let run): run.id
            case .separator(_, let id): id
            }
        }
    }

    /// One run of consecutive tool calls between two chat-visible items, as the single "N steps" row it becomes.
    struct StepsRun: Identifiable, Equatable {
        /// `steps-<first tool entry of the run, before filtering>`: stable while the run grows or is filtered.
        let id: String
        /// The run without delivery tools; never empty.
        let steps: [Entry]
        let failed: Int
        let running: Int

        /// nil when nothing is left after dropping delivery tools (the delivered item stands for them).
        init?(_ run: [Entry]) {
            guard let first = run.first else { return nil }
            let steps = run.filter { !ThreadFeed.isDelivery($0) }
            guard !steps.isEmpty else { return nil }
            self.id = "steps-\(first.id)"
            self.steps = steps
            failed = steps.filter { $0.data.status == "failed" }.count
            running = steps.filter { $0.data.status != "completed" && $0.data.status != "failed" }.count
        }

        var isRunning: Bool { running > 0 }
        /// Steps past the cap that sit under "N earlier steps" once opened.
        var hidden: Int { max(0, steps.count - ThreadFeed.visibleSteps) }
        var shown: [Entry] { Array(steps.suffix(ThreadFeed.visibleSteps)) }
    }

    /// Your own message keeps its `user-<nonce>` id when the server confirms it, so the row doesn't redraw or jump
    /// (the same rule as `ChatItem.build`).
    static func stableId(_ e: Entry) -> String {
        if e.kind == "user", let nonce = e.data.clientNonce, !nonce.isEmpty { return "user-\(nonce)" }
        return e.id
    }

    /// Chat-visible replies and tool calls. Thinking, plans and the like stay in "Full conversation".
    static func replies(_ entries: [Entry], separators: Bool = false) -> [Reply] {
        var lastDate: Date?
        var out: [Reply] = []
        var run: [Entry] = []
        var lastAuthor: String?
        func flush() {
            guard !run.isEmpty else { return }
            if let steps = StepsRun(run) { out.append(.steps(steps)) }
            run = []
        }
        for e in entries {
            if e.kind == "tool" {
                run.append(e)
                continue
            }
            guard e.isChat else { continue }
            flush()
            if separators {
                if lastDate.map({ e.date.timeIntervalSince($0) > 3600 }) ?? true {
                    out.append(.separator(e.date, id: "sep-\(stableId(e))"))
                    lastAuthor = nil
                }
                lastDate = e.date
            }
            let author = e.kind == "user" ? "user" : e.kind == "agent" ? "agent:\(e.data.author ?? "")" : e.kind
            out.append(.entry(e, groupStart: author != lastAuthor))
            lastAuthor = (author == "user" || e.kind == "agent") ? author : nil
        }
        flush()
        return out
    }

    /// Fewer replies are loaded than the host's summary counts. The summary counts a thread's user messages and final
    /// agent replies only, so loaded tool rows, thoughts and cards must not count toward it. No summary: nothing to compare.
    static func isMissingReplies(_ replies: [Entry], summary: ThreadSummary?) -> Bool {
        guard let total = summary?.count else { return false }
        return replies.filter { $0.kind == "user" || ($0.kind == "agent" && $0.data.final == true) }.count < total
    }

    /// A question waits on the reader: a permission card still pending. In a thread another writer holds it is
    /// theirs to answer, not the reader's.
    static func hasQuestion(_ replies: [Entry], held: Bool = false) -> Bool {
        !held && replies.contains { $0.kind == "permission" && $0.data.status == "pending" }
    }
}

fileprivate extension String {
    var nilIfEmpty: String? { isEmpty ? nil : self }
}
