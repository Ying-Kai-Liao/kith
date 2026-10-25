import KithKit
import SwiftUI

/// The rows a conversation is made of.

/// One chat-visible entry in a chat or a thread, with what can be done to it.
struct ChatRow: View {
    let entry: Entry
    let groupStart: Bool
    /// The bot or group the chat belongs to.
    let chat: Bot?
    let openTrace: () -> Void
    /// Main chat only: start (or open) the thread on this message.
    var openThread: ((Entry) -> Void)?
    @Environment(BotStore.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        let reply = openThread.map { open in { open(entry) } }
        // The replying agent's colour pair (in a group, the one who wrote it).
        let tint = AgentTint.resolve((entry.data.author.flatMap { model.bots[$0] } ?? chat)?.avatarColor)
        switch entry.kind {
        case "user" where BotGuestSender.label(entry, inBotGuestChat: chat?.botGuest != nil) != nil:
            // The outside bot talking: the other party, so leading with its own avatar, not Kai's bubble.
            BotSenderBubble(entry: entry, name: BotGuestSender.label(entry, inBotGuestChat: true) ?? "", provider: chat?.botGuest?.provider ?? "")
                .padding(.top, groupStart ? 12 : 4)
        case "user":
            VStack(alignment: .trailing, spacing: 4) {
                UserBubble(entry: entry, botWorking: chat?.isWorking == true, reply: reply)
                ReactionsRow(entry: entry)
                threadChip
            }
            .animation(Motion.reduced(Motion.layout, reduceMotion), value: entry.data.reactions)
            .padding(.top, groupStart ? 12 : 4)
        case "agent":
            VStack(alignment: .leading, spacing: 4) {
                if chat?.isGroup == true, groupStart { AuthorLabel(botId: entry.data.author) }
                AgentBubble(entry: entry, openTrace: openTrace, reply: reply)
                Group {
                    AnsweredByLabel(answeredBy: entry.data.answeredBy)
                    if let percent = entry.data.contextPercent {
                        Text("context \(percent)%")
                            .kithText(.secondary)
                            .foregroundStyle(Palette.muted)
                    }
                    ReactionsRow(entry: entry)
                    threadChip
                }
                .padding(.leading, chat?.isGroup == true ? 34 : 0)
            }
            .animation(Motion.reduced(Motion.layout, reduceMotion), value: entry.data.reactions)
            .padding(.top, groupStart ? 12 : 4)
        case "permission":
            VStack(alignment: .leading, spacing: 4) {
                if chat?.isGroup == true { AuthorLabel(botId: entry.data.author) }
                if entry.data.isSecretCard {
                    secretCard(tint: tint)
                } else {
                PermissionCard(entry: entry, hostName: model.hostName, tint: tint, answering: model.answering[entry.id],
                               waiting: heldElsewhere ? "Open in the terminal: answer it there."
                                   : chat?.botGuest.map({ !$0.canAnswer(isOwner: model.isOwner) }) == true ? "Only the computer's owner answers this."
                                   : GuestPrompt.waitingText(for: entry, activity: chat?.activity),
                               respond: { option in model.respond(entry, option: option) },
                               held: heldModel, stop: { model.stopHolder(for: entry) }, stopped: model.stoppedRuns[entry.id])
                }
            }
            .padding(.top, 12)
        default:
            if let request = entry.data.connectionRequest {
                ConnectionRequestCard(entry: entry, request: request).padding(.top, 12)
            } else if let card = entry.data.handoff {
                HandoffCardRow(entry: entry, card: card).padding(.top, 10)
            } else {
                // Only a notice that carries a thread (a bot from outside's conversation) gets a chip.
                VStack(alignment: .leading, spacing: 4) {
                    NoticeRow(entry: entry)
                    threadChip
                }
                .padding(.top, 10)
            }
        }
    }

    /// An Access or Key card. Old data (no `data.card`) never gets here.
    @ViewBuilder private func secretCard(tint: AgentTint) -> some View {
        let agent = entry.data.author.flatMap { model.bots[$0]?.name } ?? chat?.name ?? entry.data.agent ?? "The agent"
        if entry.data.card == "key" {
            KeyCard(entry: entry, agentName: agent, hostName: model.hostName, tint: tint, answering: model.answering[entry.id],
                    save: { account, values in try await model.provideSecret(entry, account: account, values: values) },
                    deny: { model.respond(entry, option: "deny") })
        } else {
            AccessCard(entry: entry, agentName: agent, tint: tint, answering: model.answering[entry.id]) { option in
                model.respond(entry, option: option)
            }
        }
    }

    /// The held row of an approved card whose session an old run still holds; Stop is the owner's.
    private var heldModel: HeldRunModel? {
        guard entry.data.status == "answered", let held = entry.data.held else { return nil }
        return HeldRunModel(held: held, mac: model.hostName, owner: model.isOwner, phase: model.holderPhases[held.session] ?? .idle)
    }

    /// The prompt is in a thread a terminal holds: read-only here.
    private var heldElsewhere: Bool {
        guard let root = entry.threadId else { return false }
        return model.allEntries(entry.botId).first { $0.id == root }?.data.thread?.isHeldElsewhere == true
    }

    /// What hangs under an entry: its thread, when it has replies and this view can open threads. Any kind of entry
    /// (a message, or a notice a bot from outside's conversation hangs on) gets the same chip.
    static func chipSummary(_ entry: Entry, canOpen: Bool) -> ThreadSummary? {
        guard canOpen, let summary = entry.data.thread, summary.count > 0 else { return nil }
        return summary
    }

    @ViewBuilder private var threadChip: some View {
        if let summary = Self.chipSummary(entry, canOpen: openThread != nil), let openThread {
            ThreadChip(summary: summary, yourTurn: ThreadFeed.hasQuestion(model.replies(entry.botId, root: entry.id), held: summary.isHeldElsewhere)) { openThread(entry) }
        }
    }
}

/// A chat's time separator: Today 3:27 AM.
struct TimeSeparator: View {
    let date: Date

    var body: some View {
        Text(RelativeTime.separator(date))
            .kithText(.secondary)
            .foregroundStyle(Palette.muted)
            .frame(maxWidth: .infinity)
            .padding(.top, 18)
            .padding(.bottom, 6)
    }
}

/// Who wrote a message in a group: their avatar and name in their color.
struct AuthorLabel: View {
    let botId: String?
    @Environment(BotStore.self) private var model

    var body: some View {
        let bot = botId.flatMap { model.bots[$0] }
        HStack(spacing: 8) {
            if let bot { CharacterAvatar(bot: bot, size: 26, animated: false) }
            Text(model.authorName(botId))
                .font(.subheadline.weight(.medium))
                .foregroundStyle(Palette.text)
        }
    }
}

/// Under a message with a thread: the redesign's chip (who replied, how many, how recently). Tapping either part opens it.
struct ThreadChip: View {
    let summary: ThreadSummary
    var yourTurn = false
    let open: () -> Void
    @Environment(BotStore.self) private var model

    var body: some View {
        let author = summary.authors.lazy.compactMap { model.bots[$0] }.first
        let chip = ThreadFeed.chip(face: author.map(CrewFace.init(bot:)) ?? CrewFace(portrait: nil),
                                    tint: AgentTint.resolve(author?.avatarColor), count: summary.count, unread: summary.unread,
                                    lastAt: summary.lastAt, place: nil, yourTurn: yourTurn,
                                   summary: summary, mac: model.hostName)
        ThreadChipView(chip, openLabel: "View thread, \(summary.count) \(summary.count == 1 ? "reply" : "replies")"
                           + ((summary.unread ?? 0) > 0 ? ", \(summary.unread ?? 0) new" : ""),
                       onToggle: open, onOpenThread: open)
    }
}

struct UserBubble: View {
    let entry: Entry
    let botWorking: Bool
    var reply: (() -> Void)?
    @Environment(BotStore.self) private var model

    var body: some View {
        VStack(alignment: .trailing, spacing: 4) {
            if let attachments = entry.data.attachments, !attachments.isEmpty {
                AttachmentList(attachments: attachments, botId: entry.botId)
            }
            if !(entry.data.text ?? "").isEmpty { text }
                status
        }
        .frame(maxWidth: .infinity, alignment: .trailing)
        .padding(.leading, 56)
    }

    private var text: some View {
            Text(entry.data.text ?? "")
                .kithText(.body)
                .foregroundStyle(Palette.ink)
                .textSelection(.enabled)
                .padding(.horizontal, 14)
                .padding(.vertical, 10)
                .background(Palette.bubbleUser, in: RoundedRectangle(cornerRadius: KithRadius.card + 4, style: .continuous))
                .contextActions(reactions: model.reactionPick(entry)) {
                    var items = [MenuItem("Copy", icon: "square.on.square") { Pasteboard.copy(entry.data.text) }]
                    if let reply { items.append(MenuItem("Reply in thread", icon: "arrowshape.turn.up.left", action: reply)) }
                    if entry.data.status == "queued" {
                        items.append(MenuItem("Unsend", icon: "arrow.uturn.backward") { model.unsend(entry) })
                    }
                    return items
                }
    }

    @ViewBuilder private var status: some View {
        switch entry.data.status {
        case "sending":
            Text("Sending…").font(.caption2).foregroundStyle(Palette.tertiary)
        case "queued":
            HStack(spacing: 10) {
                Text(botWorking ? "Queued until this response finishes" : "Queued").foregroundStyle(Palette.tertiary)
                Button("Unsend", systemImage: "arrow.uturn.backward") { model.unsend(entry) }
                    .labelStyle(.iconOnly)
                    .help("Unsend")
            }
            .buttonStyle(.plain)
            .font(.caption2)
        case "failed":
            HStack(spacing: 10) {
                Text("Failed to send").foregroundStyle(Palette.danger)
                Button("Resend", systemImage: "arrow.clockwise") { model.retry(entry) }.labelStyle(.iconOnly).help("Resend")
                Button("Delete", systemImage: "trash") { model.discard(entry) }.labelStyle(.iconOnly).help("Delete")
            }
            .buttonStyle(.plain)
            .font(.caption2.bold())
        case "cancelled":
            Text("Not sent").font(.caption2).foregroundStyle(Palette.tertiary)
        case "waiting":
            HStack(spacing: 10) {
                Text("Waiting for the computer to come online").foregroundStyle(Palette.tertiary)
                Button("Cancel", systemImage: "xmark.circle") { model.cancelQueued(entry) }
                    .labelStyle(.iconOnly)
                    .help("Don't send")
                    .accessibilityLabel("Don't send")
            }
            .buttonStyle(.plain)
            .font(.caption2)
        case "delivering":
            Text("Delivered to the computer").font(.caption2).foregroundStyle(Palette.tertiary)
        default:
                EmptyView()
        }
    }
}

struct AgentBubble: View {
    let entry: Entry
    let openTrace: () -> Void
    var reply: (() -> Void)?
    @Environment(BotStore.self) private var model

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Group {
                if let files = entry.data.files, !files.isEmpty {
                    ForEach(files) { file in SharedFileView(entry: entry, file: file) }
                } else {
                    MarkdownText(entry.data.text ?? "").equatable()
                        // A reply is never cut: take the height the text needs, whatever the row proposes.
                        .fixedSize(horizontal: false, vertical: true)
                        .padding(.horizontal, 14)
                        .padding(.vertical, 10)
                        .background(Palette.bubbleAgent, in: RoundedRectangle(cornerRadius: KithRadius.card + 4, style: .continuous))
                }
            }
                .contextActions(reactions: model.reactionPick(entry)) {
                    var items = [
                        MenuItem("Copy", icon: "square.on.square") { Pasteboard.copy(entry.data.text) },
                        MenuItem("Show what it did", icon: "list.bullet", action: openTrace),
                    ]
                    if let reply { items.insert(MenuItem("Reply in thread", icon: "arrowshape.turn.up.left", action: reply), at: 1) }
                    return items
                }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.trailing, 40)
    }
}


/// A message an outside bot wrote in a bot-guest chat: leading, with the bot's avatar and name. Read-only, so no reply menu.
struct BotSenderBubble: View {
    let entry: Entry
    let name: String
    let provider: String
    @Environment(BotStore.self) private var model

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            BotGuestAvatar(provider: provider, size: KithSize.botAvatarThread)
            VStack(alignment: .leading, spacing: 4) {
                Text(name).font(.subheadline.weight(.medium)).foregroundStyle(Palette.muted).lineLimit(1)
                Text(entry.data.text ?? "")
                    .kithText(.body)
                    .foregroundStyle(Palette.ink)
                    .textSelection(.enabled)
                    .padding(.horizontal, 14).padding(.vertical, 10)
                    .background(Palette.bubbleAgent, in: RoundedRectangle(cornerRadius: KithRadius.card + 4, style: .continuous))
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.trailing, 40)
        .accessibilityElement(children: .combine)
        .accessibilityLabel(Text("\(name): \(entry.data.text ?? "")"))
    }
}

struct NoticeRow: View {
    let entry: Entry
    @Environment(BotStore.self) private var model

    var body: some View {
        let text = entry.data.text ?? ""
        if let kind = ThreadFeed.writerDivider(entry, mac: model.hostName) {
            WriterDivider(kind, time: entry.date.formatted(date: .omitted, time: .shortened))
        } else if let line = VoiceCallLine.suffix(entry.data, at: entry.date) {
            Label("\(text)\(line)", systemImage: "waveform")
                .font(.footnote.monospacedDigit())
                .foregroundStyle(Palette.secondary)
                .frame(maxWidth: .infinity)
        } else {
            styled(text)
        }
    }

    @ViewBuilder private func styled(_ text: String) -> some View {
        switch entry.data.style {
        case "divider":
            HStack(spacing: 10) {
                Rectangle().fill(Palette.border).frame(height: 1)
                Text(text)
                    .font(.caption2)
                    .foregroundStyle(Palette.tertiary)
                    .multilineTextAlignment(.center)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: 260)
                    .layoutPriority(1)
                Rectangle().fill(Palette.border).frame(height: 1)
            }
            .padding(.vertical, 6)
        case "error":
            Label {
                Text(text).font(.footnote).foregroundStyle(Palette.text).textSelection(.enabled)
            } icon: {
                Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(Palette.danger)
            }
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Palette.danger.opacity(0.12), in: RoundedRectangle(cornerRadius: 12))
        default:
            Text(text)
                .font(.footnote)
                .foregroundStyle(Palette.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: .infinity)
        }
    }
}

/// The bot's live activity line: a turning orb, what it's doing and for how long. Tapping it
/// unfolds what the bot is thinking right now (the whole trace stays in "Full conversation").
struct WorkingIndicator: View {
    let bot: Bot
    /// The running turn's latest thinking, if the agent shares it.
    var thinking: String?
    @State private var expanded = false
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        HStack(alignment: .center, spacing: 10) {
            Button {
                withAnimation(Motion.reduced(Motion.layout, reduceMotion)) { expanded.toggle() }
            } label: {
                VStack(alignment: .leading, spacing: 8) {
                    HStack(spacing: 8) {
                        ThinkingOrb(state: bot.needsInput ? .listening : .working, size: 16, color: bot.needsInput ? Palette.warning : Palette.secondary)
                        Text(bot.activity.isEmpty ? "Working…" : bot.activity)
                            .font(.subheadline)
                            .foregroundStyle(bot.needsInput ? Palette.warning : Palette.secondary)
                            .lineLimit(1)
                        if let started = bot.startedAt {
                            Text(Date(milliseconds: started), style: .timer)
                                .font(.footnote.monospacedDigit())
                                .foregroundStyle(Palette.tertiary)
                        }
                        if thinking != nil {
                            Image(systemName: "chevron.down")
                                .font(.caption2.weight(.semibold))
                                .foregroundStyle(Palette.tertiary)
                                .rotationEffect(.degrees(expanded ? 180 : 0))
                        }
                    }
                    if expanded, let thinking {
                        ScrollView {
                            Text(thinking)
                                .font(.footnote)
                                .foregroundStyle(Palette.secondary)
                                .textSelection(.enabled)
                                .frame(maxWidth: .infinity, alignment: .leading)
                        }
                        .defaultScrollAnchor(.bottom)
                        .frame(maxHeight: 180)
                        .fixedSize(horizontal: false, vertical: true)
                        .transition(.opacity)
                    }
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 12)
                .background(Palette.bubbleAgent, in: RoundedRectangle(cornerRadius: 18, style: .continuous))
            }
            .buttonStyle(.plain)
            .disabled(thinking == nil)
            .accessibilityHint(thinking == nil ? "" : expanded ? "Hides its thinking" : "Shows its thinking")
            Spacer(minLength: 40)
        }
    }
}

extension BotStore {
    /// The latest thinking of the turn running in a chat (`thread` nil) or thread: the newest
    /// thought since the last message.
    func currentThinking(_ botId: String, thread: String?) -> String? {
        for e in allEntries(botId).reversed() where e.threadId == thread {
            if e.kind == "user" || (e.kind == "agent" && e.data.final == true) { return nil }
            if e.kind == "thought", let text = e.data.text, !text.isEmpty { return text }
        }
        return nil
    }
}

extension BotStore {
    /// The quick-reaction row for a message.
    func reactionPick(_ entry: Entry) -> ReactionPick {
        ReactionPick(emoji: Self.quickReactions, chosen: entry.data.reactions ?? []) { self.react(entry, $0) }
    }
}

/// The user's reactions under a message: emoji are user content, so they stay emoji. Each is yours, so each has a
/// 1 pt ink outline; tapping one takes it back. The face button adds another.
struct ReactionsRow: View {
    let entry: Entry
    @Environment(BotStore.self) private var model

    var body: some View {
        if let reactions = entry.data.reactions, !reactions.isEmpty {
            HStack(spacing: 4) {
                ForEach(reactions, id: \.self) { emoji in
                    Button { model.react(entry, emoji) } label: {
                        Text(emoji)
                            .font(.system(size: 15))
                            .padding(.horizontal, 8)
                            .padding(.vertical, 3)
                            .background(Palette.panel, in: Capsule())
                            .overlay(Capsule().strokeBorder(Palette.ink, lineWidth: 1))
                            .contentShape(Capsule())
                    }
                    .buttonStyle(PressScale())
                    .help("Remove \(emoji)")
                    .accessibilityLabel("Remove reaction \(emoji)")
                    .transition(.scale.combined(with: .opacity))
                }
                Menu {
                    ForEach(BotStore.quickReactions.filter { !reactions.contains($0) }, id: \.self) { emoji in
                        Button(emoji) { model.react(entry, emoji) }
                    }
                } label: {
                    Image(systemName: "face.smiling")
                        .font(.system(size: 14))
                        .foregroundStyle(Palette.muted)
                        .padding(.horizontal, 8)
                        .padding(.vertical, 4)
                        .background(Palette.panel, in: Capsule())
                        .contentShape(Capsule())
                }
                .accessibilityLabel("Add reaction")
            }
        }
    }
}

/// iPhone: a row renders again only when what it shows changed, so a reply being written
/// elsewhere in the chat skips it.
extension ChatRow: @MainActor Equatable {
    static func == (a: Self, b: Self) -> Bool {
        a.entry == b.entry && a.groupStart == b.groupStart && a.chat?.isGroup == b.chat?.isGroup
            && a.chat?.isWorking == b.chat?.isWorking && (a.openThread == nil) == (b.openThread == nil)
    }
}
