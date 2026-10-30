import KithKit
import SwiftUI
import UIKit

/// One endless, flat conversation with a bot or a group chat: messages, the replies of every task, and the tool
/// calls under the reply that made them, in time order. Thinking lives in the "Full conversation" sheet. Any message
/// can be replied to (a quote over the composer).
public struct ThreadView: View {
    let botId: String

    /// Set where the owner may share this agent (the header then shows Share); the app presents the sheet.
    let onShare: (() -> Void)?

    public init(botId: String, onShare: (() -> Void)? = nil) {
        self.botId = botId
        self.onShare = onShare
    }
    @Environment(BotStore.self) var model
    @Environment(\.dismiss) var dismiss
    @State var showTrace = false
    /// Runs of steps opened inline, by run id. Not persisted: a run starts collapsed.
    @State var expandedSteps: Set<String> = []
    @State var botChat: BotChatTarget?
    @State var editingGroup = false
    @State var details: DetailsRequest?
    @State var confirmNewSession = false
    @State var confirmDelete: Bot?
    @State var calling = false
    /// The chat stays on the newest message until the reader scrolls away.
    @State var following = true
    /// The reply whose time and context line is showing; scrolling closes it.
    @State var contextLineEntry: String?
    @State var loadingEarlier = false
    @Environment(\.accessibilityReduceMotion) var reduceMotion

    var bot: Bot? { model.bots[botId] }


    /// The messages (each platform's own `transcript`, in its `ThreadView+` file) and the
    /// composer, without the platform's top chrome.
    private var chat: some View {
        transcript
        .safeAreaInset(edge: .bottom) {
            VStack(spacing: 6) {
                switch ThreadFeed.chatBottom(held: ThreadFeed.heldTask(in: model.chat(botId)),
                                             mismatch: model.mismatch != nil, guest: bot?.botGuest != nil) {
                case .updateNeeded:
                    // Nothing sent now could be read on the other side.
                    if let mismatch = model.mismatch {
                        UpdateNeededCard(store: model, mismatch: mismatch)
                            .padding(.horizontal, 16)
                            .padding(.bottom, 8)
                            .transition(.opacity)
                    }
                case .writerBanner(let rootId):
                    ThreadWriterDock(botId: botId, rootId: rootId)
                case .writerBannerAndNewTask(let rootId):
                    // A terminal holds the thread the box would go on with: the box starts new tasks instead.
                    ThreadWriterDock(botId: botId, rootId: rootId)
                    Composer(botId: botId, alwaysNewTask: true)
                case .readOnly:
                    // A bot from outside's conversation: the owner reads along, nothing is sent here.
                    if let guest = bot?.botGuest { ReadOnlyLine(text: guest.readOnlyLine) }
                case .composer:
                    // The phone is in the top bar, so the dock has no call button.
                    Composer(botId: botId)
                }
            }
            .animation(Motion.reduced(Motion.layout, reduceMotion), value: model.mismatch)
        }
        // The call takes the whole screen; ending it, or leaving this chat, comes back here.
        .fullScreenCover(isPresented: $calling) {
            CallScreen(botId: botId) { calling = false }
        }
    }

    /// Why a call can't start now; nil when it can. The phone and header buttons show it.
    var callBlock: String? {
        if bot == nil { return "This chat is no longer available." }
        if model.isOffline { return "\(model.hostName) is offline." }
        if model.mismatch != nil { return "Update Kith to make calls." }
        return nil
    }

    /// One call at a time: a second tap while the screen is coming up does nothing.
    func startCall() {
        guard callBlock == nil, !calling else { return }
        calling = true
    }

    var conversation: some View {
        chrome(chat)
            .readingConversation(botId)
            .kithSheet(item: Binding<FileDownloads.Export?>(get: {
                guard let item = model.fileDownloads.export, item.botId == botId else { return nil }
                return item
            }, set: { if $0 == nil { model.fileDownloads.dismissExport() } })) { item in
                FileExportSheet(url: item.url) { model.fileDownloads.dismissExport() }
            }
            .kithSheet(isPresented: $showTrace) {
                TraceView(botId: botId)
            }
            .kithSheet(item: $botChat) { BotConversationView(botId: botId, peerId: $0.id) }
            .kithSheet(item: $details) { request in
                BotDetailsView(botId: botId, request: request, call: CallAction(blocked: callBlock) {
                    // The call screen can only rise once the sheet is gone.
                    details = nil
                    Task {
                        try? await Task.sleep(for: .milliseconds(450))
                        startCall()
                    }
                })
            }
            .kithSheet(isPresented: $editingGroup) {
                GroupEditorView(group: bot)
            }
            .kithDialog("Start a new session?", isPresented: $confirmNewSession,
                          message: "The conversation stays here, but the agent starts with a fresh context.") {
                [DialogAction("New session") { model.newSession(botId) }]
            }
            .deleteBotConfirmation($confirmDelete) { dismiss() }
    }

    // MARK: rows

    @ViewBuilder func row(_ item: ChatItem) -> some View {
        switch item.kind {
        case .separator(let date):
            TimeSeparator(date: date)
        case .exchanges(let group):
            BotMessageRow(group: group) { botChat = BotChatTarget(id: group.peerId) }
        case .steps(let run):
            StepsRow(run: run, expanded: expandedSteps.contains(run.id),
                     toggle: { toggleSteps(run.id) }, openTrace: { showTrace = true })
        case .entry(let e, let groupStart):
            if e.kind == "notice", let id = e.data.routineId {
                Button {
                    openRoutine(id)
                } label: {
                    Label(e.data.text ?? "Routine", systemImage: "clock.arrow.circlepath")
                        .font(.footnote).foregroundStyle(Palette.secondary).padding(.vertical, 10)
                }.buttonStyle(.plain)
            } else {
                entryRow(e, groupStart: groupStart)
            }
        }
    }

    func toggleSteps(_ id: String) {
        withAnimation(Motion.reduced(Motion.layout, reduceMotion)) {
            if !expandedSteps.insert(id).inserted { expandedSteps.remove(id) }
        }
    }

    func entryRow(_ e: Entry, groupStart: Bool) -> some View {
        let replyAction: ((Entry) -> Void)? = canReply(e) ? { reply($0) } : nil
        let missing = missingReplies(e)
        return ChatRow(entry: e, groupStart: groupStart, chat: bot, openTrace: { showTrace = true }, onReply: replyAction,
                       showsContext: contextLineEntry == e.id,
                       toggleContext: { contextLineEntry = ContextLineToggle.tapped(e.id, open: contextLineEntry) })
            .equatable()
            .task(id: missing) {
                // Its thread's older replies are not here yet: bring them in, so the chat has them in order.
                if missing { await model.loadThread(botId, root: e.id) }
            }
    }

    /// Reply goes to the composer, so it needs one: not a read-only chat, nor a message in a thread a terminal holds.
    func canReply(_ entry: Entry) -> Bool {
        guard bot != nil, bot?.isReadOnly != true, bot?.botGuest == nil, ReplyQuote.line(of: entry) != nil else { return false }
        guard let root = entry.threadId else { return true }
        return model.allEntries(botId).first { $0.id == root }?.data.thread?.isHeldElsewhere != true
    }

    func reply(_ entry: Entry) {
        model.replyQuotes[botId] = entry
    }

    /// A message that starts a thread whose replies were not all loaded (only live ones arrived).
    func missingReplies(_ entry: Entry) -> Bool {
        guard entry.threadId == nil, let summary = entry.data.thread else { return false }
        return ThreadFeed.isMissingReplies(model.replies(botId, root: entry.id), summary: summary)
    }

    // MARK: chrome

    /// The old connection subtitle as a chip under the name; nil while Live (nothing to say).
    var connectionChip: StatusChip.Kind? {
        switch model.shownConnection {
        case .connecting: return .catchingUp
        case .computerOffline, .offline: return .macUnreachable
        case .unauthorized: return .machineTrouble("No access")
        case .unpaired: return .machineTrouble("Not paired")
        case .online: break
        }
        return model.mismatch != nil ? .machineTrouble("Needs update") : nil
    }

    private var connectionDescription: String {
        switch model.shownConnection {
        case .connecting: return "Connecting"
        case .computerOffline, .offline: return "Offline"
        case .unauthorized: return "No access"
        case .unpaired: return "Not paired"
        case .online: break
        }
        if model.mismatch != nil { return "Needs update" }
        switch model.hostRoute {
        case .relay: return "Connected through Cloudflare"
        case .direct: return "Connected over Wi-Fi or Tailscale"
        case nil: return "Connected"
        }
    }

    func openRoutine(_ id: String?) {
        withAnimation(Motion.reduced(Motion.layout, reduceMotion)) {
            details = DetailsRequest(tab: .details, routineId: id)
        }
    }

    var menuItems: [MenuItem] {
        var items = [MenuItem("Full conversation", icon: "list.bullet.rectangle") { showTrace = true }]
        // Read-only: no settings, routines, new session or delete on a bot from outside's chat.
        if bot?.isReadOnly == true { return items }
        if let bot, bot.isGroup {
            items.append(MenuItem("Edit group", icon: "person.2") { editingGroup = true })
            items.append(MenuItem(bot.pinned ? "Unpin" : "Pin", icon: "pin") { model.setPinned(bot, !bot.pinned) })
            items.append(MenuItem("Delete group", icon: "trash", destructive: true, divider: true) { confirmDelete = bot })
            return items
        }
        if let bot {
            items.append(MenuItem("Settings", icon: "gearshape") { openSettings() })
            items.append(MenuItem(bot.pinned ? "Unpin" : "Pin", icon: "pin") { model.setPinned(bot, !bot.pinned) })
        }
        items.append(MenuItem("Routines", icon: "clock.arrow.circlepath") {
            openRoutine(nil)
        })
        items.append(MenuItem("New session", icon: "arrow.counterclockwise") { confirmNewSession = true })
        items.append(MenuItem("Delete bot", icon: "trash", destructive: true, divider: true) { confirmDelete = bot })
        return items
    }
}

// MARK: - Chat model

struct ChatItem: Identifiable {
    enum Kind {
        case separator(Date)
        case entry(Entry, groupStart: Bool)
        /// Main chat only: consecutive bot-to-bot exchanges with one peer.
        case exchanges(BotExchangeGroup)
        /// Main chat only: a run of tool calls under the reply that made it, behind one collapsed "N steps" row.
        case steps(ThreadFeed.StepsRun)
    }

    let id: String
    let kind: Kind

    /// Chat-visible entries plus time separators (gaps > 1 h) and author grouping. A bot's
    /// messages arrive whole (Grok Bot's `send_message`); what it writes along the way is trace.
    ///
    /// `groupingExchanges` builds the main chat, which is flat: every thread's replies and every tool call are
    /// interleaved with the chat's own messages by time (the host keeps its task and thread model; this is only how
    /// it is shown). Pass all of the chat's entries, threads included.
    static func build(_ entries: [Entry], groupingExchanges: Bool = false) -> [ChatItem] {
        var out: [ChatItem] = []
        var lastDate: Date?
        var lastAuthor: String?
        var tools: [Entry] = []
        // A run of tool calls becomes one collapsed row (nothing when it only delivered files).
        func flushTools() {
            if let run = ThreadFeed.StepsRun(tools) { out.append(ChatItem(id: run.id, kind: .steps(run))) }
            tools = []
        }
        let flat = groupingExchanges ? flatSlots(entries) : entries.filter(\.isChat).map { Flat.slot(.entry($0)) }
        for flatSlot in flat {
            if case .tool(let e) = flatSlot {
                tools.append(e)
                continue
            }
            guard case .slot(let slot) = flatSlot else { continue }
            flushTools()
            let date = slot.date
            // Bot-originated messages carry an empty nonce. Their entry IDs
            // distinguish them; only real nonces identify optimistic echoes.
            let id = if case .entry(let e) = slot, e.kind == "user", let nonce = e.data.clientNonce, !nonce.isEmpty { "user-\(nonce)" } else { slot.id }
            if lastDate.map({ date.timeIntervalSince($0) > 3600 }) ?? true {
                out.append(ChatItem(id: "sep-\(id)", kind: .separator(date)))
                lastAuthor = nil
            }
            lastDate = date
            switch slot {
            case .exchanges(let group):
                out.append(ChatItem(id: id, kind: .exchanges(group)))
                lastAuthor = nil
            case .entry(let e):
                // In a group each bot is its own author.
                let author = e.kind == "user" ? "user" : e.kind == "agent" ? "agent:\(e.data.author ?? "")" : e.kind
                out.append(ChatItem(id: id, kind: .entry(e, groupStart: author != lastAuthor)))
                lastAuthor = (author == "user" || e.kind == "agent") ? author : nil
            }
        }
        flushTools()
        return out
    }

    private enum Flat {
        case slot(BotExchangeGroup.Item)
        case tool(Entry)
    }

    /// The flat chat's order: the main chat's slots, every thread's chat-visible replies and every tool call, by time
    /// (ties keep arrival order). An entry that arrived both in the chat and in a thread fetch shows once.
    private static func flatSlots(_ entries: [Entry]) -> [Flat] {
        var seen = Set<String>()
        let unique = entries.filter { seen.insert(ThreadFeed.stableId($0)).inserted }
        var all: [Flat] = BotExchangeGroup.collapse(unique.filter { $0.threadId == nil }).map { .slot($0) }
        for e in unique {
            if e.kind == "tool" { all.append(.tool(e)) } else if e.threadId != nil, e.isChat { all.append(.slot(.entry(e))) }
        }
        func date(_ f: Flat) -> Date {
            switch f {
            case .slot(let s): s.date
            case .tool(let e): e.date
            }
        }
        return all.enumerated().sorted { a, b in
            let (da, db) = (date(a.element), date(b.element))
            return da == db ? a.offset < b.offset : da < db
        }.map(\.element)
    }

    /// Whether this row shows the entry (a search result's message, or a thread reply or tool call in the flat chat).
    func holds(entry id: String) -> Bool {
        switch kind {
        case let .entry(entry, _): entry.id == id || ThreadFeed.stableId(entry) == id
        case let .steps(run): run.steps.contains { $0.id == id }
        case .exchanges, .separator: false
        }
    }

    var isUserMessage: Bool {
        switch kind {
        case let .entry(entry, _): entry.kind == "user"
        default: false
        }
    }

}

/// The start of an empty group chat: who's in it and how the room works.
struct GroupIntroCard: View {
    let group: Bot
    @Environment(BotStore.self) private var model

    var body: some View {
        VStack(spacing: 12) {
            GroupAvatar(members: model.members(of: group), size: 72)
            Text(group.name).kithText(.topBar).foregroundStyle(Palette.ink)
            let names = model.members(of: group).map(\.name)
            if group.name != names.joined(separator: ", ") {
                Text(names.joined(separator: " · "))
                    .kithText(.secondary)
                    .foregroundStyle(Palette.muted)
                    .multilineTextAlignment(.center)
            }
            if !group.description.isEmpty {
                Text(group.description)
                    .kithText(.secondary)
                    .foregroundStyle(Palette.muted)
                    .multilineTextAlignment(.center)
            }
            Text("Everyone answers in turn. @mention a bot to ask just that one.")
                .kithText(.secondary)
                .foregroundStyle(Palette.muted)
                .multilineTextAlignment(.center)
                .padding(.top, 4)
        }
        .frame(maxWidth: .infinity)
        .padding(.horizontal, 24)
    }
}

struct IntroCard: View {
    let bot: Bot
    @Environment(BotStore.self) private var model

    var body: some View {
        VStack(spacing: 12) {
            BotAvatar(bot: bot, size: 72)
            Text(bot.name).kithText(.agentName).foregroundStyle(Palette.ink)
            Text(bot.managedWorkspace ? "\(model.backendName(bot.backend)) · Personal workspace" : "\(model.backendName(bot.backend)) in \(bot.cwd)")
                .kithText(.secondary)
                .foregroundStyle(Palette.muted)
                .multilineTextAlignment(.center)
            if !bot.description.isEmpty {
                Text(bot.description)
                    .kithText(.secondary)
                    .foregroundStyle(Palette.muted)
                    .multilineTextAlignment(.center)
            }
            Text("Tell it what you need. You'll get a notification when it's done or needs you.")
                .kithText(.secondary)
                .foregroundStyle(Palette.muted)
                .multilineTextAlignment(.center)
                .padding(.top, 4)
        }
        .frame(maxWidth: .infinity)
        .padding(.horizontal, 24)
    }
}

extension ThreadView {
    public var body: some View {
        conversation
            .onChange(of: model.pendingThread, initial: true) { _, pending in
                guard let pending, pending.chat == botId else { return }
                model.pendingThread = nil
                // The same path as a search result: the list waits for the row, scrolls to it and flashes it once.
                model.focusEntry = BotStore.FocusEntry(chat: botId, entry: pending.root)
            }
            // The threads the reader has unread replies in are on screen too: read them.
            .background {
                ForEach(readThreads, id: \.self) { Color.clear.readingConversation(botId, thread: $0) }
            }
    }

    /// Roots of the threads this chat shows replies of that have something unread, and the current task.
    private var readThreads: [String] {
        let roots = model.chat(botId).filter { ($0.data.thread?.unread ?? 0) > 0 }.map(\.id)
        return Array(Set(roots + [Tasks.current(in: model.chat(botId))?.id].compactMap { $0 }))
    }

    /// What is working in this chat: the main conversation, the current task's lane, or any other lane of the chat
    /// (an older task still going). The chat's one working line shows it.
    private var workingLane: WorkingLane? {
        guard let bot, !model.isOffline else { return nil }
        let lanes = [WorkingLane(chat: botId, thread: nil), WorkingLane(chat: botId, thread: Tasks.current(in: model.chat(botId))?.id)]
        if let lane = lanes.first(where: { bot.isWorking(in: $0.chat, thread: $0.thread) }) { return lane }
        return bot.workingLanes?.first { $0.chat == botId }
    }

    /// iPhone's messages: all this device has, earlier ones from the computer as the reader
    /// scrolls up; the chat follows the newest message until they scroll away, and a reply is
    /// revealed steadily as it's written.
    var transcript: some View {
        let items = ChatItem.build(model.allEntries(botId), groupingExchanges: true)
        var rows: [ConversationRow] = []
        if moreOnComputer {
            rows.append(ConversationRow("earlier") {
                Spinner(size: 14)
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 14)
                    .accessibilityLabel("Loading earlier messages")
                    // A chat shorter than the screen never scrolls, so showing the row is the trigger.
                    .onAppear { showEarlier() }
            })
        }
        if items.isEmpty, let bot {
            rows.append(ConversationRow("intro") {
                if bot.isGroup { GroupIntroCard(group: bot).padding(.top, 40) } else { IntroCard(bot: bot).padding(.top, 40) }
            })
        }
        rows += items.map { item in ConversationRow(item.id, isUserMessage: item.isUserMessage) { row(item) } }
        // Offline, "working" is only what the computer last said; don't show it as live.
        if let bot, let lane = workingLane {
            rows.append(ConversationRow("working") {
                WorkingIndicator(bot: bot, thinking: model.currentThinking(botId, thread: lane.thread))
                    .padding(.top, 6)
            })
        }
        // The row holding the message a search result pointed at.
        let focus = model.focusEntry.flatMap { $0.chat == botId ? $0.entry : nil }
            .flatMap { entry in items.first { $0.holds(entry: entry) }?.id }
        // Pull to refresh reconnects, like Reconnect in the computer menu, until the link is back.
        return ConversationList(rows: rows, following: $following, onUserScroll: { contextLineEntry = nil }, focus: focus, focused: { model.focusEntry = nil }, nearTop: { showEarlier() }, refresh: {
            model.restartStream()
            _ = try? await model.ready()
        })
    }

    private var moreOnComputer: Bool {
        model.canLoadOlder(botId)
    }

    /// The reader scrolled up to the top: fetch earlier messages from the computer.
    private func showEarlier() {
        guard moreOnComputer, !loadingEarlier else { return }
        loadingEarlier = true
        Task {
            await model.loadOlder(botId)
            loadingEarlier = false
        }
    }

    /// The sky strip and top bar replace the system navigation bar; the folder chip sits under it.
    func chrome(_ content: some View) -> some View {
        content
            .hidesSystemNavigationBar()
            .background { Palette.background.ignoresSafeArea() }
            .safeAreaInset(edge: .top, spacing: 0) {
                VStack(spacing: 4) {
                    topBar
                    if bot?.isGroup != true, model.placeFolder(botId, thread: nil) != nil {
                        PlaceFolderChip(botId: botId)
                            .frame(maxWidth: .infinity)
                            .padding(.horizontal, KithSpacing.gutter)
                    }
                }
                .padding(.bottom, 4)
                .background(alignment: .top) {
                    // Rows scroll under the bar. The sky is cut to the bar's own height (its image is taller than the bar)
                    // and fades out over the bottom of it, so messages stay visible right under the cloud.
                    Palette.background.ignoresSafeArea(edges: .top)
                        .overlay(alignment: .top) {
                            SkyBackground(.header)
                                .mask(alignment: .top) {
                                    LinearGradient(stops: [.init(color: .black, location: 0.7), .init(color: .clear, location: 1)],
                                                   startPoint: .top, endPoint: .bottom)
                                }
                        }
                        .clipped()
                        .ignoresSafeArea(edges: .top)
                        .overlay(alignment: .bottom) {
                            LinearGradient(colors: [Palette.background, Palette.background.opacity(0)], startPoint: .top, endPoint: .bottom)
                                .frame(height: 12).offset(y: 12).allowsHitTesting(false)
                        }
                }
            }
    }

    /// Back, the bot's picture and name (they open the agent sheet), the phone and the ••• menu. Under the name: a
    /// chip when the computer isn't Live, or "using the computer" while the agent has the screen.
    private var topBar: some View {
        // The name sits in the middle of the bar; the narrow layout (small phones, long names) gives the buttons room first.
        ViewThatFits(in: .horizontal) {
            ZStack {
                HStack(spacing: 8) { backButton; Spacer(minLength: 0); callButton; menuButton }
                titleButton.padding(.horizontal, 108)
            }
            HStack(spacing: 8) { backButton; titleButton.frame(maxWidth: .infinity); callButton; menuButton }
        }
        .padding(.horizontal, KithSpacing.gutter)
        .padding(.top, 4)
    }

    private var backButton: some View { TopGlassButton("Back", systemImage: "chevron.left") { dismiss() } }

    @ViewBuilder private var callButton: some View {
        if bot?.isReadOnly != true { callTopButton }
    }

    private var callTopButton: some View {
        TopGlassButton("Call", systemImage: "phone.fill", action: startCall)
            .disabled(callBlock != nil || calling)
            .opacity(callBlock != nil ? 0.4 : 1)
            .accessibilityHint(callBlock ?? "")
            .help(callBlock ?? "Call \(bot?.name ?? "")")
    }

    private var menuButton: some View {
        TopGlassMenu("\(bot?.name ?? "Conversation") actions", systemImage: "ellipsis") { menuContent }
    }

    /// The picture and name open the agent sheet; the status chip sits under them.
    private var titleButton: some View {
        VStack(spacing: 2) {
            Button(action: openDetails) {
                HStack(spacing: 8) {
                    if let bot {
                        if bot.isGroup {
                            GroupAvatar(members: model.members(of: bot), size: KithSize.botAvatarTopBar)
                        } else {
                            BotAvatar(bot: bot, size: KithSize.botAvatarTopBar)
                        }
                    }
                    Text(bot?.name ?? "")
                        .kithText(bot?.isGroup == true ? .topBar : .agentName)
                        .foregroundStyle(Palette.ink)
                        .lineLimit(1)
                        .truncationMode(.tail)
                }
                .frame(minHeight: 44)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("\(bot?.name ?? "Conversation"), details")
            statusLine
        }
    }

    /// Whether this bot is operating the computer's screen right now.
    private var usingComputer: Bool { model.screen?.agentBot == botId }

    @ViewBuilder private var statusLine: some View {
        if usingComputer {
            Button { model.screenRequest = ScreenRequest(watching: botId) } label: {
                Label("using the computer", systemImage: "display")
                    .kithText(.secondary).foregroundStyle(Palette.muted)
                    .symbolEffect(.pulse, options: .repeating)
                    .frame(minHeight: 44)
            }
            .buttonStyle(.plain)
            .accessibilityHint("Shows the computer's screen")
        } else if let kind = connectionChip {
            StatusChip(kind)
                .help("\(model.hostName) · \(connectionDescription)")
        } else if let kind = ThreadFeed.writerChip(ThreadFeed.heldTask(in: model.chat(botId))?.data.thread, mac: model.hostName) {
            StatusChip(kind, tint: AgentTint.resolve(bot?.avatarColor))
        }
    }

    @ViewBuilder private var menuContent: some View {
        Text("\(model.hostName) · \(model.connectionLabel)")
        Button("Reconnect", systemImage: "arrow.clockwise") { model.restartStream() }
        if model.screen != nil {
            Button("Show the computer", systemImage: "display") {
                model.screenRequest = usingComputer ? ScreenRequest(watching: botId) : ScreenRequest()
            }
        }
        if bot?.isGroup != true, bot?.isReadOnly != true, let root = Tasks.current(in: model.chat(botId)), let bot {
            Button("Copy resume command", systemImage: "terminal") { Pasteboard.copy(root.resumeCommand(agent: bot.name)) }
        }
        Divider()
        if let onShare, bot?.isReadOnly != true {
            Button("Share access…", systemImage: "person.badge.plus", action: onShare)
        }
        if bot?.isReadOnly != true { Button("Details", systemImage: "info.circle", action: openDetails) }
        ForEach(menuItems) { item in
            if item.divider { Divider() }
            Button(role: item.destructive ? .destructive : nil, action: item.action) {
                if let icon = item.icon { Label(item.title, systemImage: icon) } else { Text(item.title) }
            }
        }
    }

    private func openDetails() {
        guard bot?.isReadOnly != true else { return }
        if bot?.isGroup == true { editingGroup = true } else { details = DetailsRequest() }
    }

    func openSettings() {
        details = DetailsRequest(tab: .settings)
    }
}
