import KithKit
import SwiftUI

public extension BotStore {
    /// The owner's peek at one handoff; nil when the computer has no record of it (an ask from before the records).
    func peekHandoff(_ id: String) async -> HandoffPeek? {
        try? await withLink { try await $0.peekHandoff(id) }
    }

    /// The handoffs still going or waiting on the owner. Empty for a member and for a computer before the peek.
    func openHandoffs() async -> [HandoffPeek] {
        guard isOwner else { return [] }
        return (try? await withLink { try await $0.openHandoffs() }) ?? []
    }
}

/// The owner's peek at a handoff: the brief, the worker's live line and the limits it runs under, with Join (open the
/// worker's thread), Stop and a box to the worker. It reads every few seconds while it is open; closing the sheet ends
/// the task. Owner only: the callers don't offer it to a member.
public struct HandoffSheet: View {
    let store: BotStore
    let id: String
    @Environment(\.dismissModal) private var dismiss
    @Environment(AccountStore.self) private var accounts: AccountStore?
    @State private var peek: HandoffPeek?
    @State private var loaded = false
    @State private var confirmStop = false
    @State private var stopping = false
    @State private var draft = ""
    @State private var sent = false
    @FocusState private var typing: Bool

    static let refresh: Duration = .seconds(4)

    public init(store: BotStore, id: String) {
        self.store = store
        self.id = id
    }

    public var body: some View {
        VStack(spacing: 0) {
            ModalHeader(peek?.title ?? "Handoff") {
                if let peek { Text(stopping && peek.isOpen ? "Stopping" : peek.stateLabel).kithText(.secondary).foregroundStyle(Palette.muted) }
            }
            if let peek {
                content(peek)
            } else if loaded {
                Text("There is no record of this handoff. It may be from before handoffs were kept.")
                    .kithText(.body).foregroundStyle(Palette.muted)
                    .padding(KithSpacing.gutter)
                Spacer(minLength: 0)
            } else {
                ProgressView().padding(40)
                Spacer(minLength: 0)
            }
        }
        .background(Palette.background)
        .task(id: id) {
            while !Task.isCancelled {
                let next = await store.peekHandoff(id)
                if !Task.isCancelled { peek = next; loaded = true }
                try? await Task.sleep(for: Self.refresh)
            }
        }
        .kithDialog("Stop this handoff?", isPresented: $confirmStop,
                    message: "\(peek?.to ?? "The agent") stops what it is doing. You can pick the conversation up again from its thread.") {
            [DialogAction("Stop", destructive: true) { stop() }]
        }
    }

    private func content(_ peek: HandoffPeek) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                if let brief = peek.brief {
                    Text(brief).kithText(.body).foregroundStyle(Palette.ink).textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(12)
                        .background(Palette.panel, in: RoundedRectangle(cornerRadius: KithRadius.rowTool, style: .continuous))
                }
                if peek.state == .needsYou {
                    Text("It is waiting on you. The question is a card in its room.").kithText(.secondary).foregroundStyle(Palette.muted)
                }
                if let live = peek.live {
                    HStack(alignment: .firstTextBaseline, spacing: 8) {
                        Text(peek.isOpen ? "NOW" : "LAST").kithText(.label).foregroundStyle(Palette.muted)
                        Text(live.text).kithText(live.isTool ? .code : .secondary).foregroundStyle(Palette.ink)
                    }
                }
                limits(peek)
                times(peek)
                if peek.sendBlocked == nil { box(peek) }
                if sent { Text("Sent to its thread.").kithText(.secondary).foregroundStyle(Palette.muted) }
                actions(peek)
            }
            .padding(KithSpacing.gutter)
        }
        .scrollDismissesKeyboard(.interactively)
    }

    /// The limits line: a budget bar and "$0.70 / $3", hops, crossings, elapsed; the one closest to its limit is in ink.
    @ViewBuilder private func limits(_ peek: HandoffPeek) -> some View {
        let parts = peek.limitParts
        if !parts.isEmpty {
            VStack(alignment: .leading, spacing: 8) {
                Divider()
                HStack(spacing: 14) {
                    ForEach(Array(parts.enumerated()), id: \.offset) { _, part in
                        HStack(spacing: 8) {
                            if part.kind == .budget, let fraction = part.fraction {
                                Capsule().fill(Palette.line).frame(width: 72, height: 4)
                                    .overlay(alignment: .leading) {
                                        Capsule().fill(part.watch ? Palette.ink : Palette.muted).frame(width: 72 * fraction)
                                    }
                            }
                            Text(part.label).kithText(.code)
                                .fontWeight(part.watch ? .semibold : .regular)
                                .foregroundStyle(part.watch ? Palette.ink : Palette.muted)
                        }
                    }
                }
            }
            .accessibilityElement(children: .combine)
            .accessibilityLabel(Text("Limits: " + parts.map(\.label).joined(separator: ", ")))
        }
    }

    private func times(_ peek: HandoffPeek) -> some View {
        HStack(spacing: 14) {
            Text("Started \(Self.clock(peek.startedMs))")
            if let end = peek.answeredMs { Text("\(peek.state == .done ? "Done" : "Ended") \(Self.clock(end))") }
            if let capped = peek.cappedMs { Text("Capped \(Self.clock(capped))") }
        }
        .kithText(.secondary).foregroundStyle(Palette.muted)
    }

    private func box(_ peek: HandoffPeek) -> some View {
        HStack(alignment: .bottom, spacing: 8) {
            TextField("Message \(peek.to)", text: $draft, axis: .vertical)
                .lineLimit(1...5)
                .focused($typing)
                .padding(.horizontal, 12).padding(.vertical, 10)
                .background(Palette.panel, in: RoundedRectangle(cornerRadius: KithRadius.rowTool, style: .continuous))
                .onChange(of: draft) { sent = false }
            Button("Send") { send(peek) }
                .buttonStyle(SecondaryButtonStyle())
                .disabled(draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
        }
    }

    private func actions(_ peek: HandoffPeek) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                Button("Join") { join(peek) }
                    .buttonStyle(SecondaryButtonStyle())
                    .disabled(peek.joinBlocked != nil)
                Button(stopping ? "Stopping" : "Stop") { confirmStop = true }
                    .buttonStyle(SecondaryButtonStyle())
                    .disabled(peek.stopBlocked != nil || stopping)
            }
            if let why = peek.joinBlocked ?? peek.stopBlocked {
                Text(why).kithText(.secondary).foregroundStyle(Palette.muted)
            }
        }
    }

    // MARK: actions

    /// Opens the worker's thread in the app: its chat, scrolled to the thread. No write to the computer.
    private func join(_ peek: HandoffPeek) {
        guard let thread = peek.thread else { return }
        store.pendingThread = thread.threadId.map { PendingThread(chat: thread.botId, root: $0) }
        dismiss()
        if let accounts {
            accounts.selection = BotReference(accountId: accounts.selection?.accountId, computerId: store.computer.id, botId: thread.botId)
        }
    }

    /// One Stop only: a second tap while the first settles sends nothing.
    private func stop() {
        guard let thread = peek?.thread, !stopping else { return }
        stopping = true
        store.stop(thread.botId, thread: thread.threadId)
    }

    private func send(_ peek: HandoffPeek) {
        let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let thread = peek.thread, !text.isEmpty else { return }
        store.send(text, to: thread.botId, thread: thread.threadId)
        draft = ""
        sent = true
    }

    static func clock(_ ms: Int64) -> String {
        Date(timeIntervalSince1970: Double(ms) / 1000).formatted(date: .omitted, time: .shortened)
    }
}
