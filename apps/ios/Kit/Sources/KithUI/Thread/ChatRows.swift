import KithKit
import SwiftUI

/// The rows a conversation is made of.

/// Which reply's context line is open: a tap opens that one (closing any other) or closes it if open.
enum ContextLineToggle {
    static func tapped(_ id: String, open: String?) -> String? { open == id ? nil : id }
}

/// One chat-visible entry in a chat or a thread, with what can be done to it.
struct ChatRow: View {
    let entry: Entry
    let groupStart: Bool
    /// The bot or group the chat belongs to.
    let chat: Bot?
    let openTrace: () -> Void
    /// Reply to this message (a quote over the composer); nil where nothing is sent (read-only chats).
    var onReply: ((Entry) -> Void)?
    /// "Make a topic from here" on this message; nil unless the owner may (see `ThreadView.canMakeTopic`).
    var onMakeTopic: ((Entry) -> Void)?
    @Environment(BotStore.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    /// A tapped agent reply shows its time and context line under it. The chat holds which one is open
    /// (one at a time, closed by scrolling), so the row only gets whether it is this one.
    var showsContext = false
    var toggleContext: () -> Void = {}
    /// A late reply: the first line of the message it answers, when that is known.
    var lateQuote: String?

    var body: some View {
        let reply = onReply.map { reply in { reply(entry) } }
        let topicAction = onMakeTopic.map { make in { make(entry) } }
        // The replying agent's colour pair (in a group, the one who wrote it).
        let tint = AgentTint.resolve((entry.data.author.flatMap { model.bots[$0] } ?? chat)?.avatarColor)
        switch entry.kind {
        case "user" where BotGuestSender.label(entry, inBotGuestChat: chat?.botGuest != nil) != nil:
            // The outside bot talking: the other party, so leading with its own avatar, not Kai's bubble.
            BotSenderBubble(entry: entry, name: BotGuestSender.label(entry, inBotGuestChat: true) ?? "", provider: chat?.botGuest?.provider ?? "")
                .padding(.top, groupStart ? 12 : 4)
        case "user" where chat?.isGroup == true && entry.isRoomBotPost:
            // An outside bot someone brought into the room: its own face, name in Inter and the Bot tag.
            RoomBotRow(entry: entry, groupStart: groupStart, lateQuote: lateQuote)
                .padding(.top, groupStart || entry.data.isLate ? 12 : 4)
        case "user":
            VStack(alignment: .trailing, spacing: 4) {
                UserBubble(entry: entry, botWorking: chat?.isWorking == true, reply: reply, makeTopic: topicAction)
                ReactionsRow(entry: entry)
            }
            .animation(Motion.reduced(Motion.layout, reduceMotion), value: entry.data.reactions)
            .padding(.top, groupStart ? 12 : 4)
        case "agent":
            VStack(alignment: .leading, spacing: 4) {
                if groupStart || entry.data.isLate { AuthorLabel(botId: entry.data.author ?? chat?.id, late: entry.data.isLate) }
                if let card = entry.data.richCard, card.drawsInChat {
                    if card.type == "news" { NewsCard(entry: entry, card: card) } else { ScheduleCard(entry: entry, card: card) }
                } else {
                    AgentBubble(entry: entry, openTrace: openTrace, reply: reply, makeTopic: topicAction, late: entry.data.isLate, lateQuote: lateQuote) {
                        withAnimation(Motion.reduced(Motion.layout, reduceMotion)) { toggleContext() }
                    }
                }
                Group {
                    if entry.data.isLate { LateReasonLine(reason: entry.data.lateReason) }
                    AnsweredByLabel(answeredBy: entry.data.answeredBy)
                    if showsContext, entry.data.status != "streaming" {
                        ContextLineView(time: Self.clock(entry.date), percent: entry.data.contextPercent)
                            .frame(maxWidth: .infinity, alignment: .trailing)
                            .padding(.trailing, 40)
                            .transition(.opacity)
                    }
                    ReactionsRow(entry: entry)
                }
                .padding(.leading, chat?.isGroup == true ? 34 : 0)
            }
            .animation(Motion.reduced(Motion.layout, reduceMotion), value: entry.data.reactions)
            .padding(.top, groupStart || entry.data.isLate ? 12 : 4)
        case "permission":
            VStack(alignment: .leading, spacing: 4) {
                if chat?.isGroup == true { AuthorLabel(botId: entry.data.author) }
                if let fold = CardFold.of(entry) {
                    FoldedCard(fold: fold, time: Self.clock(entry.date)) { permissionBody(tint: tint) }
                } else {
                    permissionBody(tint: tint)
                }
            }
            .padding(.top, 12)
        default:
            if let request = entry.data.connectionRequest {
                ConnectionRequestCard(entry: entry, request: request).padding(.top, 12)
            } else if let card = entry.data.handoff {
                HandoffCardRow(entry: entry, card: card).padding(.top, 10)
            } else if let card = entry.data.richCard, card.type == "schedule", card.drawsInChat {
                // A routine's card arrives as a notice from the host, not as the bot's message.
                ScheduleCard(entry: entry, card: card).padding(.top, 10)
            } else {
                NoticeRow(entry: entry).padding(.top, 10)
            }
        }
    }

    private static func clock(_ date: Date) -> String {
        date.formatted(.dateTime.hour(.twoDigits(amPM: .omitted)).minute(.twoDigits))
    }

    /// The full approval, limit or key/access card.
    @ViewBuilder private func permissionBody(tint: AgentTint) -> some View {
        if entry.data.isSecretCard {
            secretCard(tint: tint)
        } else if entry.data.limit != nil {
            // An agent at its monthly limit (the owner's message waits under it), or a person at theirs.
            LimitCardView(entry: entry, tint: tint, answering: model.answering[entry.id],
                          waiting: GuestPrompt.waitingText(for: entry, activity: chat?.activity, isOwner: model.isOwner),
                          time: Self.clock(entry.date),
                          respond: { option in model.respond(entry, option: option) })
        } else {
            PermissionCard(entry: entry, hostName: model.hostName, tint: tint, answering: model.answering[entry.id],
                           waiting: heldElsewhere ? "Open in the terminal: answer it there."
                           : chat?.botGuest.map({ !$0.canAnswer(isOwner: model.isOwner) }) == true ? "Only the computer's owner answers this."
                           : GuestPrompt.waitingText(for: entry, activity: chat?.activity, isOwner: model.isOwner),
                           respond: { option in model.respond(entry, option: option) },
                           held: heldModel, stop: { model.stopHolder(for: entry) }, stopped: model.stoppedRuns[entry.id])
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
    /// The reply came after its run was over: the Late tag follows the name.
    var late = false
    @Environment(BotStore.self) private var model

    var body: some View {
        let bot = botId.flatMap { model.bots[$0] }
        HStack(spacing: 8) {
            if let bot { CharacterAvatar(bot: bot, size: 24, animated: false).shadow(color: Palette.ink.opacity(0.12), radius: 4, y: 2) }
            Text(model.authorName(botId))
                .kithText(.agentName)
                .foregroundStyle(Palette.ink)
            if late { LateTag() }
        }
    }
}

struct UserBubble: View {
    let entry: Entry
    let botWorking: Bool
    var reply: (() -> Void)?
    var makeTopic: (() -> Void)?
    @State private var selecting = false
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
                    var items = [
                        MenuItem("Copy", icon: "square.on.square") { Pasteboard.copy(entry.data.text) },
                        MenuItem("Select Text", icon: "selection.pin.in.out") { selecting = true },
                    ]
                    if let reply { items.insert(MenuItem("Reply", icon: "arrowshape.turn.up.left", action: reply), at: 0) }
                    if let makeTopic { items.append(MenuItem("Make a topic from here", detail: "This and the replies after it", icon: "rectangle.stack", action: makeTopic)) }
                    if entry.data.status == "queued" {
                        items.append(MenuItem("Unsend", icon: "arrow.uturn.backward") { model.unsend(entry) })
                    }
                    return items
                }
                .kithSheet(isPresented: $selecting) { SelectTextSheet(text: entry.data.text ?? "") }
    }

    @ViewBuilder private var status: some View {
        switch entry.data.status {
        case "sending":
            Text("Sending…").font(.caption2).foregroundStyle(Palette.tertiary)
        case "queued":
            HStack(spacing: 10) {
                Text(UserStatusLine.queued(waitsFor: entry.data.waitsFor, botWorking: botWorking)).foregroundStyle(Palette.tertiary)
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
    var makeTopic: (() -> Void)?
    /// A late reply: outlined, with the line it answers when that is known.
    var late = false
    var lateQuote: String?
    /// A tap on the bubble (links keep their own).
    var onTap: (() -> Void)?
    @State private var selecting = false
    @Environment(BotStore.self) private var model

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Group {
                if let files = entry.data.files, !files.isEmpty {
                    let parts = ImageGridPlan.split(files)
                    if parts.images.count >= 2 {
                        ImageGrid(images: parts.images.map { file in
                            GalleryImage(id: file.id, name: file.name) { [model, entry] in
                                await model.fileDownloads.bytes(client: model.client, entry: entry, file: file)
                            }
                        }, fill: Palette.bubbleAgent)
                        ForEach(parts.others) { file in SharedFileView(entry: entry, file: file) }
                    } else {
                        ForEach(files) { file in SharedFileView(entry: entry, file: file) }
                    }
                } else if late {
                    LateBubble(quote: lateQuote) {
                        MarkdownText(entry.data.text ?? "").equatable().fixedSize(horizontal: false, vertical: true)
                    }
                } else {
                    MarkdownText(entry.data.text ?? "").equatable()
                        // A reply is never cut: take the height the text needs, whatever the row proposes.
                        .fixedSize(horizontal: false, vertical: true)
                        .padding(.horizontal, 14)
                        .padding(.vertical, 10)
                        .background(Palette.bubbleAgent, in: RoundedRectangle(cornerRadius: KithRadius.card + 4, style: .continuous))
                }
            }
                .onTapGesture { onTap?() }
                .contextActions(reactions: model.reactionPick(entry)) {
                    var items = [
                        MenuItem("Copy", icon: "square.on.square") { Pasteboard.copy(entry.data.text) },
                        MenuItem("Select Text", icon: "selection.pin.in.out") { selecting = true },
                        MenuItem("Show what it did", icon: "list.bullet", action: openTrace),
                    ]
                    if let reply { items.insert(MenuItem("Reply", icon: "arrowshape.turn.up.left", action: reply), at: 0) }
                    if let makeTopic { items.append(MenuItem("Make a topic from here", detail: "This and the replies after it", icon: "rectangle.stack", action: makeTopic)) }
                    return items
                }
                .kithSheet(isPresented: $selecting) { SelectTextSheet(text: MarkdownText.plain(entry.data.text ?? "")) }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.trailing, 40)
    }
}


/// An outside bot's message in a group room. Late ones are outlined and say why; unprompted ones look like any other.
struct RoomBotRow: View {
    let entry: Entry
    let groupStart: Bool
    var lateQuote: String?

    var body: some View {
        let late = entry.data.isLate
        VStack(alignment: .leading, spacing: 4) {
            if let sender = entry.data.roomBot, groupStart || late { RoomBotLabel(sender: sender, late: late) }
            if late {
                LateBubble(quote: lateQuote) { text }
                LateReasonLine(reason: entry.data.lateReason)
            } else {
                text
                    .padding(.horizontal, 14).padding(.vertical, 10)
                    .background(Palette.bubbleAgent, in: RoundedRectangle(cornerRadius: KithRadius.card + 4, style: .continuous))
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.trailing, 40)
    }

    private var text: some View {
        Text(entry.data.text ?? "").kithText(.body).foregroundStyle(Palette.ink).textSelection(.enabled)
            .fixedSize(horizontal: false, vertical: true)
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
        if entry.isRoomNotice {
            RoomNoticeLine(entry: entry)
        } else if let kind = ThreadFeed.writerDivider(entry, mac: model.hostName) {
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
                ForEach(ReactionGroups.of(reactions), id: \.emoji) { group in
                    let emoji = group.emoji
                    Button { model.react(entry, emoji) } label: {
                        HStack(spacing: 4) {
                            Text(emoji).font(.system(size: 15))
                            Text("\(group.count)")
                                .font(.system(size: 13).monospacedDigit())
                                .foregroundStyle(Palette.ink)
                        }
                            .padding(.horizontal, 8)
                            .padding(.vertical, 3)
                            .background(Palette.panel, in: Capsule())
                            .overlay(Capsule().strokeBorder(Palette.ink, lineWidth: 1))
                            .contentShape(Capsule())
                    }
                    .buttonStyle(PressScale())
                    .help("Remove \(emoji)")
                    .accessibilityLabel("\(emoji) \(group.count)")
                    .accessibilityHint("Removes your reaction")
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

/// The line under a tapped agent reply: time, a 13 pt ring and "N% context". No percent, time only.
struct ContextLineView: View {
    let time: String
    let percent: Int?

    var body: some View {
        HStack(spacing: 6) {
            Text(time).kithText(.code).foregroundStyle(Palette.muted)
            if let percent {
                ZStack {
                    Circle().stroke(Palette.ink.opacity(0.16), lineWidth: 2)
                    Circle().trim(from: 0, to: ContextLine.fraction(percent))
                        .stroke(Palette.ink, style: StrokeStyle(lineWidth: 2, lineCap: .butt))
                        .rotationEffect(.degrees(-90))
                }
                .frame(width: 13, height: 13)
                Text(ContextLine.label(percent: percent) ?? "").kithText(.code).foregroundStyle(Palette.muted)
            }
        }
        .accessibilityElement(children: .combine)
    }
}

/// iPhone: a row renders again only when what it shows changed, so a reply being written
/// elsewhere in the chat skips it.
extension ChatRow: @MainActor Equatable {
    static func == (a: Self, b: Self) -> Bool {
        a.entry == b.entry && a.groupStart == b.groupStart && a.chat?.isGroup == b.chat?.isGroup
            && a.chat?.isWorking == b.chat?.isWorking && (a.onReply == nil) == (b.onReply == nil)
            && a.showsContext == b.showsContext && a.lateQuote == b.lateQuote
            && (a.onMakeTopic == nil) == (b.onMakeTopic == nil)
    }
}
