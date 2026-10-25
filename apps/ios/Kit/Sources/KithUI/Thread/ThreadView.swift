import KithKit
import SwiftUI
import UIKit

/// One endless conversation with a bot or a group chat. Only deliberate messages show
/// here; tool calls and thinking live in the "Full conversation" sheet. Any message can
/// start a thread, which opens over it.
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
    @State var openThread: ThreadTarget?
    @State var botChat: BotChatTarget?
    @State var editingGroup = false
    @State var details: DetailsRequest?
    @State var confirmNewSession = false
    @State var confirmDelete: Bot?
    @State var calling = false
    /// The chat stays on the newest message until the reader scrolls away.
    @State var following = true
    @State var loadingEarlier = false
    /// Tasks the user opened or folded on this device; the rest follow their state.
    @Environment(\.accessibilityReduceMotion) var reduceMotion

    var bot: Bot? { model.bots[botId] }


    func closeThread() {
        withAnimation(Motion.reduced(Motion.layout, reduceMotion)) { openThread = nil }
    }

    /// The messages (each platform's own `transcript`, in its `ThreadView+` file) and the
    /// composer, without the platform's top chrome.
    private var chat: some View {
        transcript
        .safeAreaInset(edge: .bottom) {
            VStack(spacing: 6) {
                if let mismatch = model.mismatch {
                    // Nothing sent now could be read on the other side.
                    UpdateNeededCard(store: model, mismatch: mismatch)
                        .padding(.horizontal, 16)
                        .padding(.bottom, 8)
                        .transition(.opacity)
                } else if let held = ThreadFeed.heldTask(in: model.chat(botId)) {
                    // A terminal holds the thread the box would go on with.
                    ThreadWriterDock(botId: botId, rootId: held.id)
                } else if let guest = bot?.botGuest {
                    // A bot from outside's conversation: the owner reads along, nothing is sent here.
                    ReadOnlyLine(text: guest.readOnlyLine)
                } else {
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
                guard let item = model.fileDownloads.export, item.botId == botId,
                      item.isPresentedByChat(openThread: openThread?.id) else { return nil }
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
        case .task(let root):
            if let bot {
                TaskRow(root: root, bot: bot, newest: root.id == Tasks.current(in: model.chat(botId))?.id,
                        choice: Binding(get: { model.taskChoices[root.id] }, set: { model.taskChoices[root.id] = $0 })) {
                    showTrace = true
                } openThread: {
                    withAnimation(Motion.reduced(Motion.layout, reduceMotion)) { openThread = ThreadTarget(id: root.id) }
                }
            } else {
                // Without its bot (deleted) a task is just its first message.
                ChatRow(entry: root, groupStart: true, chat: bot) { showTrace = true }
            }
        case .entry(let e, let groupStart):
            if e.kind == "notice", let id = e.data.routineId {
                Button {
                    openRoutine(id)
                } label: {
                    Label(e.data.text ?? "Routine", systemImage: "clock.arrow.circlepath")
                        .font(.footnote).foregroundStyle(Palette.secondary).padding(.vertical, 10)
                }.buttonStyle(.plain)
            } else {
                ChatRow(entry: e, groupStart: groupStart, chat: bot) {
                    showTrace = true
                } openThread: { root in
                    withAnimation(Motion.reduced(Motion.layout, reduceMotion)) { openThread = ThreadTarget(id: root.id) }
                }
                    .equatable()
            }
        }
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
        /// Main chat only: a task's first message, shown with its thread or as a card (`TaskRow`).
        case task(Entry)
    }

    let id: String
    let kind: Kind

    /// Chat-visible entries plus time separators (gaps > 1 h) and author grouping. A bot's
    /// messages arrive whole (Grok Bot's `send_message`); what it writes along the way is trace.
    static func build(_ entries: [Entry], groupingExchanges: Bool = false) -> [ChatItem] {
        var out: [ChatItem] = []
        var lastDate: Date?
        var lastAuthor: String?
        var slots = groupingExchanges ? BotExchangeGroup.collapse(entries) : entries.filter(\.isChat).map(BotExchangeGroup.Item.entry)
        // A task still talking after the chat's last message sits at the end, not at its old root.
        var moved: Set<String> = []
        if groupingExchanges { (slots, moved) = Tasks.place(slots) }
        for slot in slots {
            var date = slot.date
            if case .entry(let e) = slot, moved.contains(e.id) { date = Date(timeIntervalSince1970: Double(Tasks.lastAt(e)) / 1000) }
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
            case .entry(let e) where groupingExchanges && e.isTask:
                out.append(ChatItem(id: id, kind: .task(e)))
                lastAuthor = nil
            case .entry(let e):
                // In a group each bot is its own author.
                let author = e.kind == "user" ? "user" : e.kind == "agent" ? "agent:\(e.data.author ?? "")" : e.kind
                out.append(ChatItem(id: id, kind: .entry(e, groupStart: author != lastAuthor)))
                lastAuthor = (author == "user" || e.kind == "agent") ? author : nil
            }
        }
        return out
    }

    var isUserMessage: Bool {
        switch kind {
        case let .entry(entry, _): entry.kind == "user"
        case .task: true
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
                openThread = ThreadTarget(id: pending.root)
            }
            .kithSheet(item: $openThread) { target in
                RepliesView(botId: botId, rootId: target.id) { closeThread() }
            }
    }

    private var working: Bool { bot?.isWorking(in: botId, thread: nil) == true && !model.isOffline }

    /// iPhone's messages: all this device has, earlier ones from the computer as the reader
    /// scrolls up; the chat follows the newest message until they scroll away, and a reply is
    /// revealed steadily as it's written.
    var transcript: some View {
        let thread = model.chat(botId)
        let items = ChatItem.build(thread, groupingExchanges: true)
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
        if let bot, working {
            rows.append(ConversationRow("working") {
                WorkingIndicator(bot: bot, thinking: model.currentThinking(botId, thread: nil))
                    .padding(.top, 6)
            })
        }
        // Pull to refresh reconnects, like Reconnect in the computer menu, until the link is back.
        return ConversationList(rows: rows, following: $following, nearTop: { showEarlier() }, refresh: {
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
