import KithKit
import SwiftUI

/// An approval or a question from the agent, as the redesign's decision card: what it wants, where it runs, and the
/// answers as buttons in the agent's colour pair. Answering calls `respond` exactly as before; the card disables
/// every button while an answer is on its way, so a double tap sends once.
struct PermissionCard: View {
    let entry: Entry
    let hostName: String
    /// The agent's colour pair (neutral grey for an old colour).
    var tint: AgentTint = .neutral
    /// The option whose answer is on its way to the computer, if any.
    var answering: String?
    /// Set when the owner has to answer (a guest's chat): "Waiting for <name>" in place of the buttons.
    var waiting: String?
    let respond: (String?) -> Void
    /// Answered, with an old run still holding the session: the row under the outcome (nil when not held).
    var held: HeldRunModel?
    var stop: () -> Void = {}
    /// The divider after the owner stopped the old run from this card.
    var stopped: StoppedRun?
    @State private var expanded = false
    @State private var confirming = false
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private var d: EntryData { entry.data }
    private var pending: Bool { d.status == "pending" }

    private var headline: String {
        switch d.toolKind {
        case "execute": "Wants to run a command"
        case "edit", "delete", "move": "Wants to change files"
        case "fetch": "Wants to access the web"
        case "read", "search": "Wants to read files"
        default: "Wants to use a tool"
        }
    }

    private var place: String {
        "Runs on \(hostName)\(d.cwd.map { " · \(($0 as NSString).lastPathComponent)" } ?? "")\(d.chain.map { " · \($0)" } ?? "")"
    }

    /// The card's values; the option ids are the host's.
    var cardModel: DecisionCardModel {
        DecisionCardModel(heading: place, question: headline, detail: d.title, options: ordered.map { o in
            DecisionOption(id: o.optionId, title: label(o), role: o.kind == "allow_once" ? .primary : o.kind.hasPrefix("allow") ? .normal : .destructive)
        })
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if pending, waiting == nil {
                DecisionCard(cardModel, tint: tint, answering: answering) { respond($0) }
            } else {
                VStack(alignment: .leading, spacing: 6) {
                    Text(place).kithText(.label).foregroundStyle(Palette.muted)
                    Text(headline).kithText(.body).fontWeight(.semibold).foregroundStyle(Palette.ink)
                    if let title = d.title, !title.isEmpty {
                        Text(title).kithText(.code).foregroundStyle(Palette.muted).lineLimit(expanded ? nil : 3)
                    }
                    Text(waiting ?? outcome + (held == nil ? "" : " · waiting to go on"))
                        .kithText(.secondary).fontWeight(.medium).foregroundStyle(Palette.muted)
                    if let held {
                        HeldRunRow(model: held, onStop: { confirming = true })
                            .transition(.opacity)
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(14)
                .background(Palette.panel, in: RoundedRectangle(cornerRadius: KithRadius.card, style: .continuous))
            }
            if let stopped {
                StoppedRunDivider(text: HeldRunModel.stoppedDivider(kind: stopped.kind, at: stopped.at), symbol: HeldRunModel.symbol(stopped.kind))
            }
            if hasDetail {
                Disclosure(isExpanded: $expanded) {
                    VStack(alignment: .leading, spacing: 8) {
                        if let command = d.command, !command.isEmpty {
                            CodeBox(text: command)
                        }
                        if let detail = d.detail, !detail.isEmpty {
                            CodeBox(text: String(detail.prefix(2000)))
                        }
                        ForEach(d.diffs ?? [], id: \.self) { DiffView(diff: $0) }
                    }
                    .padding(.top, 6)
                } label: {
                    Text("Details").kithText(.secondary).fontWeight(.medium).foregroundStyle(Palette.muted)
                }
                .padding(.horizontal, 4)
            }
        }
        .padding(.trailing, 40)
        .animation(Motion.reduced(Motion.layout, reduceMotion), value: held)
        .kithDialog(held?.confirmTitle ?? "", isPresented: $confirming, message: held?.confirmMessage(agent: d.agent ?? "the agent")) {
            [DialogAction("Stop", destructive: true) { stop() }]
        }
        // The old run ended by itself (or another device stopped it): the open confirm has nothing left to ask.
        .onChange(of: held == nil) { _, gone in if gone { confirming = false } }
    }

    private var hasDetail: Bool {
        !(d.command ?? "").isEmpty || !(d.detail ?? "").isEmpty || !(d.diffs ?? []).isEmpty
    }

    /// Ordered like Grok Bot: Allow once, Always allow, then Deny.
    private var ordered: [PermissionOption] {
        let rank = ["allow_once": 0, "allow_always": 1, "reject_once": 2, "reject_always": 3]
        return (d.options ?? []).sorted { (rank[$0.kind] ?? 9) < (rank[$1.kind] ?? 9) }
    }

    private func label(_ o: PermissionOption) -> String {
        switch o.kind {
        case "allow_once": "Allow once"
        case "allow_always": "Always allow"
        case "reject_once": "Deny"
        case "reject_always": "Never"
        default: o.name
        }
    }

    private var outcome: String {
        switch d.status {
        case "answered":
            // An "always" kith-auth couldn't keep stayed a once grant; the host says so in its own words.
            if let said = d.outcome, !said.isEmpty { return said }
            let chosen = d.options?.first { $0.optionId == d.selected }
            return switch chosen?.kind {
            case "allow_once": "Allowed once"
            case "allow_always": "Always allowed"
            case "reject_once": "Denied"
            case "reject_always": "Never allowed"
            default: chosen?.name ?? "Answered"
            }
        case "cancelled": return "Cancelled"
        default: return "Expired — the agent moved on"
        }
    }
}

struct CodeBox: View {
    let text: String

    var body: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            Text(text)
                .font(.system(.caption, design: .monospaced))
                .foregroundStyle(Palette.text)
                .textSelection(.enabled)
                .padding(8)
        }
        .frame(maxHeight: 240)
        .background(Palette.codeBackground, in: RoundedRectangle(cornerRadius: 8))
    }
}

struct DiffView: View {
    let diff: FileDiff

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 6) {
                Image(systemName: diff.isNew ? "doc.badge.plus" : "doc.text")
                Text((diff.path as NSString).lastPathComponent).lineLimit(1)
                Spacer()
                Text("+\(diff.added)").foregroundStyle(Palette.added)
                Text("−\(diff.removed)").foregroundStyle(Palette.removed)
            }
            .font(.caption.monospaced())
            .foregroundStyle(Palette.secondary)
            ScrollView(.horizontal, showsIndicators: false) {
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(Array(diff.patch.split(separator: "\n", omittingEmptySubsequences: false).prefix(80).enumerated()), id: \.offset) { _, line in
                        Text(line.isEmpty ? " " : String(line))
                            .foregroundStyle(line.hasPrefix("+") ? Palette.added : line.hasPrefix("-") ? Palette.removed : Palette.secondary)
                    }
                }
                .font(.system(.caption2, design: .monospaced))
                .padding(8)
            }
            .frame(maxHeight: 260)
            .background(Palette.codeBackground, in: RoundedRectangle(cornerRadius: 8))
        }
    }
}
