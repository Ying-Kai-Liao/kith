import Foundation
import KithKit

/// A run of an agent's own 1:1 chat that someone or something else prompted, with the agent's replies to it.
/// It shows as one line (`FoldReason.othersRun`); `items` are the rows it opens to.
struct OthersRunGroup {
    let id: String
    let items: [ChatItem]
    let reason: FoldReason

    func holds(entry id: String) -> Bool { items.contains { $0.holds(entry: id) } }
}

/// Finds the runs in a 1:1 chat's rows. Pure, so it is tested without a view.
///
/// Who prompted comes from the host (`data.prompter`). Own prompts (`app`, `terminal`) end a run; so does any user
/// entry without a known prompter (an old entry, or one this app does not know), which never folds. A run lasts from
/// an others prompt (`manager`, `schedule`, `agent`, `person`) until the next own prompt.
enum OthersRun {
    enum Source: Equatable {
        case own, others, unknown

        init(_ entry: Entry) {
            switch entry.data.prompter?.kind {
            case "app", "terminal": self = .own
            case "manager", "schedule", "agent", "person": self = .others
            default: self = .unknown
            }
        }
    }

    /// An ask or a warning: it stays out of the fold and shows under the fold line, so the run never hides what needs
    /// the person (a pending permission or key card, an unanswered question, an error, a failed reply).
    static func isAsk(_ entry: Entry) -> Bool {
        switch entry.kind {
        case "permission": return entry.data.status == "pending"
        case "agent": return entry.data.unanswered == true || ["failed", "error"].contains(entry.data.status ?? "")
        case "notice": return entry.data.style == "error"
        default: return false
        }
    }

    /// `items` with each run folded to one item. `working`: the agent is working now, so a run that ends the chat is live.
    static func fold(_ items: [ChatItem], working: Bool) -> [ChatItem] {
        var out: [ChatItem] = []
        var run: Builder?
        var held: [ChatItem] = []

        func close(live: Bool = false) {
            if let built = run {
                out.append(built.item(live: live))
                out += built.asks
            }
            // A time separator that came inside the run belongs after it, unless the run went on.
            out += held
            run = nil
            held = []
        }

        for item in items {
            switch item.kind {
            case .separator:
                if run != nil { held.append(item) } else { out.append(item) }
            case let .entry(entry, _):
                if entry.kind == "user" {
                    switch Source(entry) {
                    case .others:
                        // A different prompter starts its own line, so each title names who it was.
                        if let current = run?.prompters.last, current != entry.data.prompter { close() }
                        held = []
                        if run == nil { run = Builder(firstId: item.id) }
                        run?.add(item, prompter: entry.data.prompter)
                    case .own, .unknown:
                        close()
                        out.append(item)
                    }
                } else if run != nil {
                    held = []
                    if isAsk(entry) {
                        run?.asks.append(item)
                    } else {
                        run?.add(item, prompter: nil, counts: entry.kind == "agent")
                    }
                } else {
                    out.append(item)
                }
            case .steps:
                if run != nil {
                    held = []
                    run?.add(item, prompter: nil, counts: false)
                } else {
                    out.append(item)
                }
            case .exchanges, .topic, .task, .othersRun:
                close()
                out.append(item)
            }
        }
        // A reply still being written keeps the run going.
        let streaming = run?.items.contains { item in
            if case let .entry(e, _) = item.kind { return e.data.status == "streaming" }
            return false
        } ?? false
        close(live: working || streaming)
        return out
    }

    private struct Builder {
        let firstId: String
        var items: [ChatItem] = []
        var asks: [ChatItem] = []
        var prompters: [EntryPrompter] = []
        var messages = 0

        init(firstId: String) { self.firstId = firstId }

        mutating func add(_ item: ChatItem, prompter: EntryPrompter?, counts: Bool = true) {
            items.append(item)
            if let prompter {
                messages += 1
                if !prompters.contains(prompter) { prompters.append(prompter) }
            } else if counts {
                messages += 1
            }
        }

        func item(live: Bool) -> ChatItem {
            let reason = FoldReason.othersRun(title: OthersRun.title(prompters, live: live), count: messages, live: live)
            // The first message names the run, so new entries arriving into it keep its row.
            let id = "run-\(firstId)"
            return ChatItem(id: id, kind: .othersRun(OthersRunGroup(id: id, items: items, reason: reason)))
        }
    }

    /// "Worked with coder", "Routine · daily digest", "Worked with a flow manager"; "Working with" while it goes on.
    static func title(_ prompters: [EntryPrompter], live: Bool) -> String {
        guard let first = prompters.first else { return live ? "Working with others" : "Worked with others" }
        let verb = live ? "Working with" : "Worked with"
        let more = prompters.count > 1 ? " and others" : ""
        let name = first.name.flatMap { $0.isEmpty ? nil : $0 }
        switch first.kind {
        case "schedule": return (name.map { "Routine · \($0)" } ?? "Routine") + more
        case "manager": return "\(verb) a flow manager" + more
        case "agent": return "\(verb) \(name ?? "another agent")" + more
        default: return "\(verb) \(name ?? "someone")" + more
        }
    }

    /// "1 message", "6 messages".
    static func countText(_ count: Int) -> String { count == 1 ? "1 message" : "\(count) messages" }
}
