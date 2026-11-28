import KithKit
import SwiftUI

/// The pages an agent's page opens (mock keys a:, agadv:, agkeys:, perms:, aga:, and the memory and routines lists).
enum AgentSubpage: Hashable {
    case advanced, keys, permissions, people, memory, routines

    var title: String {
        switch self {
        case .advanced: "Advanced"
        case .keys: "Keys"
        case .permissions: "Permissions"
        case .people: "People"
        case .memory: "Memory"
        case .routines: "Routines"
        }
    }
}

/// A kith agent's page, owner only: the face, then one scroll of rows, and Advanced for the rest. Replaces the old
/// four-tab sheet (Details, Memory, Access, Settings) for an agent; a guest and any other bot keep that sheet.
struct AgentPageView: View {
    let botId: String
    let call: CallAction?
    private let routineId: String?
    @Environment(BotStore.self) private var model
    @Environment(AccountStore.self) private var accounts: AccountStore?
    @Environment(\.dismissModal) private var dismiss
    @State private var subpage: AgentSubpage?
    @State private var draft: BotDraft?
    @State private var saveError: String?
    @State private var saving: Task<Void, Never>?
    @State private var kith: KithInfoStore?
    @State private var access: AccessStore?
    @State private var usage: AgentUsage?
    @State private var usageFailed = false
    @State private var pickingLook = false
    @State private var editingInstructions = false
    @State private var showingUsage = false
    @State private var editingLimit = false
    @State private var template: EditorRequest?

    init(botId: String, request: DetailsRequest, call: CallAction?) {
        self.botId = botId
        self.call = call
        routineId = request.routineId
        _subpage = State(initialValue: request.routineId != nil ? .routines : request.subpage ?? Self.subpage(for: request.tab))
    }

    /// The old sheet's tab, as the page that holds its things now.
    static func subpage(for tab: DetailsTab) -> AgentSubpage? {
        switch tab {
        case .details: nil
        case .memory: .memory
        case .access: .permissions
        case .settings: .advanced
        }
    }

    private var bot: Bot? { model.bots[botId] }
    private var agent: String { bot?.hqAgent ?? "" }
    private var name: String { bot?.name ?? "this agent" }
    private var shown: BotSections { BotSections(info: kith?.info, unsupportedHost: kith?.unavailable ?? true) }
    private var offline: Bool { model.isOffline }

    var body: some View {
        Group {
            if let subpage { page(subpage) } else { root }
        }
        .background(Palette.background)
        .task(id: botId) { draft = bot.map(BotDraft.init) }
        .task(id: "\(botId)/\(agent)") {
            guard !agent.isEmpty else { kith = nil; return }
            let store = kith?.botId == botId ? kith! : KithInfoStore(botId: botId) { try await model.ready() }
            kith = store
            await store.load()
        }
        .task(id: "access/\(botId)/\(agent)") {
            guard !agent.isEmpty else { access = nil; return }
            let store = access?.agent == agent ? access! : AccessStore(agent: agent) { try await model.ready() }
            access = store
            await store.load()
        }
        .task(id: "usage/\(agent)") { await loadUsage() }
        .onChange(of: draft) { _, draft in autosave(draft) }
        .kithSheet(isPresented: $pickingLook) {
            if let binding = Binding($draft) { ColourSheet(draft: binding, editsName: true) }
        }
        .kithSheet(isPresented: $editingInstructions) {
            if let binding = Binding($draft) { InstructionsEditor(text: binding.description) }
        }
        .kithSheet(isPresented: $showingUsage) {
            AgentSpendSheet(store: model, agent: agent) { Task { await loadUsage() } }
        }
        .kithSheet(isPresented: $editingLimit) {
            if let usage {
                LimitSheet(store: model, view: usage, cancel: { editingLimit = false }) { saved in
                    self.usage = saved
                    editingLimit = false
                }
            }
        }
        .kithSheet(item: $template) { BotTemplateView(draft: $0.draft) }
    }

    // MARK: the page

    private var root: some View {
        ScrollView {
            VStack(spacing: 22) {
                if let bot {
                    face(bot)
                    rows(bot)
                }
            }
            .padding(.horizontal, 16)
            .padding(.bottom, 28)
            .environment(\.cardStyle, .outlined)
            .environment(\.noteLines, 2)
        }
        .scrollDismissesKeyboard(.interactively)
        .modifier(KithRefresh(store: kith))
        .overlay(alignment: .topTrailing) { closeButton }
    }

    private var closeButton: some View {
        HStack(spacing: 8) {
            if let bot {
                Button { template = EditorRequest(BotDraft(bot)) } label: {
                    Label("Create template", systemImage: "square.on.square")
                        .font(.system(size: 12, weight: .medium))
                        .foregroundStyle(Palette.ink)
                        .padding(.horizontal, 12)
                        .padding(.vertical, 7)
                        .background(Palette.panel, in: Capsule())
                }
                .buttonStyle(.plain)
            }
            Spacer(minLength: 0)
            Button { dismiss() } label: {
                Image(systemName: "xmark")
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(Palette.muted)
                    .frame(width: 28, height: 28)
                    .background(Palette.panel, in: Circle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Close details")
        }
        .padding(.top, 14)
        .padding(.horizontal, 16)
    }

    /// Avatar with the pencil, the name, one line about it, then Message, Call and Share.
    private func face(_ bot: Bot) -> some View {
        VStack(spacing: 4) {
            CharacterAvatar(bot: bot, size: 76)
                .padding(.bottom, 8)
                .overlay(alignment: .bottomTrailing) {
                    Button { pickingLook = true } label: {
                        Image(systemName: "pencil")
                            .font(.system(size: 11, weight: .semibold))
                            .foregroundStyle(Palette.ink)
                            .frame(width: 26, height: 26)
                            .background(Palette.panel, in: Circle())
                            .overlay(Circle().strokeBorder(Palette.background, lineWidth: 2))
                    }
                    .buttonStyle(.plain)
                    .offset(x: 6, y: -6)
                    .disabled(draft == nil)
                    .accessibilityLabel("Edit name and look")
                }
            Text(bot.name).kithText(.agentName).foregroundStyle(Palette.ink).multilineTextAlignment(.center)
            if let line = oneLine(bot) {
                Text(line)
                    .font(.system(size: 12.5))
                    .foregroundStyle(Palette.muted)
                    .multilineTextAlignment(.center)
                    .lineLimit(2)
            }
            actions.padding(.top, 12)
        }
        .frame(maxWidth: .infinity)
        .padding(.top, 64)
    }

    /// What the agent is, in a line: kith's description, else the one on the bot.
    private func oneLine(_ bot: Bot) -> String? {
        let text = kith?.info?.description ?? bot.description
        return text.isEmpty ? nil : text
    }

    private var actions: some View {
        VStack(spacing: 6) {
            HStack(spacing: 10) {
                pill("Message", "bubble.left.fill") { dismiss() }
                if let call {
                    pill("Call", "phone.fill", enabled: call.blocked == nil, action: call.start)
                }
            }
            if let reason = call?.blocked {
                Text(reason).font(.system(size: 11.5)).foregroundStyle(Palette.muted)
            }
        }
    }

    private func pill(_ title: String, _ icon: String, enabled: Bool = true, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Label(title, systemImage: icon)
                .font(.system(size: 13.5, weight: .semibold))
                .foregroundStyle(Palette.ink)
                .padding(.horizontal, 16)
                .padding(.vertical, 9)
                .background(Palette.panel, in: Capsule())
                .opacity(enabled ? 1 : 0.4)
        }
        .buttonStyle(PressScale())
        .disabled(!enabled)
    }

    // MARK: rows

    @ViewBuilder private func rows(_ bot: Bot) -> some View {
        if offline {
            SheetNote(text: "\(model.hostName) is offline. Rows that need it are off until it's back.")
        }
        CardSection {
            navRow("Usage", icon: "chart.bar", value: usageLine, enabled: !offline && usage != nil) { showingUsage = true }
            instructionsRow
            navRow("Asks before", icon: "checkmark.shield", value: AgentPermissions.modeLabel(kith?.info) ?? "…",
                   enabled: access?.available != false) { subpage = .permissions }
            navRow("People", icon: "person.2", value: peopleValue, enabled: access?.available != false) { subpage = .people }
            navRow("Limit", icon: "gauge.with.dots.needle.33percent", value: limitValue,
                   enabled: !offline && usage != nil) { editingLimit = true }
            navRow("Memory", icon: "brain", value: "What it knows") { subpage = .memory }
        }
        if let draft = Binding($draft) {
            CardSection {
                SwitchRow("Notifications", detail: "When it needs you or finishes",
                          isOn: Binding(get: { draft.wrappedValue.notify ?? true }, set: { draft.wrappedValue.notify = $0 }))
                if model.screen != nil {
                    SwitchRow("Use the computer", detail: useComputerDetail,
                              isOn: Binding(get: { draft.wrappedValue.computer ?? false }, set: { draft.wrappedValue.computer = $0 }))
                }
            }
        }
        CardSection {
            navRow("Advanced", icon: "slider.horizontal.3", value: "Model, routines") { subpage = .advanced }
        }
        if let saveError {
            Text(saveError).font(.footnote).foregroundStyle(Palette.danger).frame(maxWidth: .infinity, alignment: .leading)
        }
    }

    private func navRow(_ title: String, icon: String, value: String, enabled: Bool = true, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            SheetRow(icon: icon, title: title, value: value, chevron: true)
        }
        .buttonStyle(.plain)
        .disabled(!enabled)
        // Still loading is not off: only a computer that is away dims the row.
        .opacity(enabled || !offline ? 1 : 0.5)
    }

    /// kith's role prompt opens in place; an agent with none gets the standing instructions editor.
    @ViewBuilder private var instructionsRow: some View {
        if let prompt = kith?.info?.role?.prompt, !shown.instructions {
            KithInstructionsRow(text: prompt)
        } else {
            Button { editingInstructions = true } label: {
                SheetRow(icon: "doc.text", title: "Instructions",
                         detail: (draft?.description.isEmpty ?? true) ? "Rules that always apply" : String(draft?.description.prefix(90) ?? ""),
                         chevron: true)
            }
            .buttonStyle(.plain)
            .disabled(draft == nil)
        }
    }

    private var usageLine: String {
        guard let usage else { return usageFailed ? "Unavailable" : "…" }
        return "\(Spend.money(usage.month)) · \(usage.monthLabel)"
    }

    private var limitValue: String {
        guard let usage else { return usageFailed ? "Unavailable" : "…" }
        return usage.limit.map { Spend.money($0) } ?? "None"
    }

    private var peopleValue: String {
        guard case .loaded(let grants) = access?.ledger else { return "" }
        let n = AccessTab.people(grants).count
        return n == 0 ? "Only you" : n == 1 ? "1 person" : "\(n) people"
    }

    private var useComputerDetail: String {
        model.screen?.controlReady != true
            ? "Set up Computer access in Kith on that computer first."
            : "Let it see the screen and use the mouse and keyboard."
    }

    // MARK: subpages

    private func page(_ subpage: AgentSubpage) -> some View {
        VStack(spacing: 0) {
            ScreenHeader {
                BackButton { withAnimation(Motion.layout) { self.subpage = nil } }
            } title: {
                Text(subpage.title).font(.body.weight(.semibold)).foregroundStyle(Palette.ink)
            } trailing: {
                IconButton("Close", systemImage: "xmark") { dismiss() }
            }
            ScrollView {
                content(subpage)
                    .padding(.horizontal, 16)
                    .padding(.top, 8)
                    .padding(.bottom, 28)
                    .environment(\.cardStyle, .outlined)
                    .environment(\.noteLines, 2)
            }
            .scrollDismissesKeyboard(.interactively)
            .modifier(KithRefresh(store: kith))
        }
    }

    @ViewBuilder private func content(_ subpage: AgentSubpage) -> some View {
        switch subpage {
        case .advanced:
            if let draft = Binding($draft) {
                AgentAdvancedPage(name: name, draft: draft, kith: kith, access: access, shown: shown,
                                  open: { self.subpage = $0 }, askBuilder: builderAsk)
            }
        case .keys:
            if let access { AgentKeysPage(store: access) }
        case .permissions:
            if let access { AgentPermissionsPage(name: name, store: access, kith: kith) { ask(botId, $0) } }
        case .people:
            if let access { AccessTabView(store: access, kith: kith).people }
        case .memory:
            HqMemoryCard(botId: botId, kith: kith)
        case .routines:
            RoutinesView(botId: botId, initialId: routineId, leave: { dismiss() }, kithSchedules: kith?.info?.schedules ?? [],
                         schedulesUnreadable: kith?.info?.errors["schedules"] != nil, botName: name)
        }
    }

    // MARK: asking in chat

    /// builder's chat, when this computer has one.
    private var builder: Bot? { model.agentBot("builder") }

    private var builderAsk: ((String) -> Void)? {
        guard builder != nil else { return nil }
        return { askBuilder($0) }
    }

    private func askBuilder(_ text: String) {
        guard let builder else { return }
        ask(builder.id, text)
    }

    /// Puts `text` in the box of `chat` and opens it. Nothing is sent: the person reads it and sends.
    private func ask(_ chat: String, _ text: String) {
        let current = model.composerDraft(for: chat)
        model.setComposerDraft(current.isEmpty ? text : current + "\n" + text, for: chat)
        if chat != botId, let accounts {
            accounts.selection = BotReference(accountId: accounts.selection?.accountId, computerId: model.computer.id, botId: chat)
        }
        dismiss()
    }

    // MARK: loading and saving

    private func loadUsage() async {
        guard !agent.isEmpty else { return }
        #if DEBUG
        if let demo = DemoAgent.usage { usage = demo; return }
        #endif
        do {
            usage = try await model.ready().agentUsage(agent, period: .month)
            usageFailed = false
        } catch {
            if usage == nil { usageFailed = true }
        }
    }

    /// Saves a moment after the last change; a refusal keeps the old value and says why.
    private func autosave(_ draft: BotDraft?) {
        saving?.cancel()
        guard let draft, let bot, draft != BotDraft(bot), draft.isValid else { return }
        saving = Task {
            try? await Task.sleep(for: .milliseconds(600))
            guard !Task.isCancelled else { return }
            do {
                _ = try await model.save(draft.normalized)
                saveError = nil
            } catch {
                saveError = "Not saved: \(error.localizedDescription)"
                if let bot = model.bots[botId] { self.draft = BotDraft(bot) }
            }
        }
    }
}

/// Pull to refresh asks kith again (`refresh: true`); other bots keep a page that doesn't pull.
struct KithRefresh: ViewModifier {
    let store: KithInfoStore?

    @ViewBuilder func body(content: Content) -> some View {
        if let store {
            content.refreshable { await store.load(refresh: true) }
        } else {
            content
        }
    }
}
