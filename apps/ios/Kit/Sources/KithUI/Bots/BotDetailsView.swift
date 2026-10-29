import KithKit
import SwiftUI

/// The details sheet's pages, as on the desktop inspector.
enum DetailsTab: Hashable {
    case details, memory, access, settings
}

/// What to open the details sheet on.
struct DetailsRequest: Identifiable {
    let id = UUID()
    var tab: DetailsTab = .details
    /// A routine to open in its editor (Details page).
    var routineId: String?
}

/// The details page's Call button: `blocked` is why it can't be used now.
struct CallAction {
    var blocked: String?
    let start: () -> Void
}

/// A bot's details (Grok Bot's): who it is, then its details, memory and settings. Settings save
/// as you edit.
struct BotDetailsView: View {
    let botId: String
    let routineId: String?
    @State var tab: DetailsTab
    @Environment(BotStore.self) private var model
    @Environment(\.dismissModal) private var dismiss
    @State private var draft: BotDraft?
    @State private var saveError: String?
    @State private var saving: Task<Void, Never>?
    @State private var template: EditorRequest?
    /// A kith bot's details (serve/docs/bot-info.md); nil for any other bot. Kept while `bot` events arrive.
    @State private var kith: KithInfoStore?
    /// The Access tab's data (keys, grants, spend); nil for a bot with no hq agent, a guest, or a group.
    @State private var access: AccessStore?
    private let call: CallAction?
    /// Cards that kith's sections repeat go once kith has the fact on screen.
    private var shown: BotSections { BotSections(hqAgent: bot?.hqAgent, info: kith?.info, unsupportedHost: kith?.unavailable ?? true) }

    init(botId: String, request: DetailsRequest, call: CallAction? = nil) {
        self.botId = botId
        self.call = call
        routineId = request.routineId
        _tab = State(initialValue: request.tab)
    }

    private var bot: Bot? { model.bots[botId] }

    /// Where the identity's bottom edge is in the sheet; once it has scrolled off, a mini header takes over.
    @State private var identityBottom: CGFloat = 200
    private var scrolled: Bool { identityBottom < 24 }

    var body: some View {
        ScrollView {
            LazyVStack(spacing: 0, pinnedViews: [.sectionHeaders]) {
                if let bot {
                    identity(bot)
                        .background(GeometryReader { proxy in
                            Color.clear.preference(key: IdentityBottomKey.self, value: proxy.frame(in: .named("agentSheet")).maxY)
                        })
                }
                Section {
                    page.padding(.top, 14)
                        .padding(.horizontal, 16)
                        .padding(.bottom, 24)
                        .environment(\.cardStyle, .outlined)
                        .environment(\.noteLines, 2)
                } header: {
                    pinnedHeader
                }
            }
        }
        .coordinateSpace(name: "agentSheet")
        .onPreferenceChange(IdentityBottomKey.self) { identityBottom = $0 }
        .scrollDismissesKeyboard(.interactively)
        .modifier(KithRefresh(store: kith))
        .background(Palette.background)
        .overlay(alignment: .topTrailing) { actions }
        .task(id: botId) { draft = bot.map(BotDraft.init) }
        .task(id: "\(botId)/\(bot?.hqAgent ?? "")") {
            guard let bot, let agent = bot.hqAgent, !bot.isGroup, !agent.isEmpty else { kith = nil; return }
            let store = kith?.botId == botId ? kith! : KithInfoStore(botId: botId) { try await model.ready() }
            kith = store
            await store.load()
        }
        .task(id: "access/\(botId)/\(bot?.hqAgent ?? "")") {
            guard model.isOwner, let bot, let agent = bot.hqAgent, !bot.isGroup, !agent.isEmpty else { access = nil; return }
            let store = access?.agent == agent ? access! : AccessStore(agent: agent) { try await model.ready() }
            access = store
            await store.load()
        }
        .onChange(of: draft) { _, draft in autosave(draft) }
        .kithSheet(item: $template) { BotTemplateView(draft: $0.draft) }
    }

    /// Create template at the top left, close at the top right; the pill leaves once the identity scrolls away.
    private var actions: some View {
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
                .opacity(scrolled ? 0 : 1)
                .allowsHitTesting(!scrolled)
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
            .help("Close details")
        }
        .padding(.top, 14)
        .padding(.horizontal, 16)
    }

    /// The tabs, with a small bot and name above them once the identity has scrolled away.
    private var pinnedHeader: some View {
        VStack(spacing: 10) {
            if scrolled, let bot {
                HStack(spacing: 7) {
                    CharacterAvatar(bot: bot, size: 24, animated: false)
                    Text(bot.name).kithText(.agentName).foregroundStyle(Palette.ink)
                }
                .padding(.top, 14)
                .transition(.opacity)
            }
            SegmentedChoice(selection: $tab, options: tabs)
                .padding(.horizontal, 16)
        }
        .padding(.top, scrolled ? 0 : 18)
        .padding(.bottom, 10)
        .frame(maxWidth: .infinity)
        .background(Palette.background)
        .animation(.easeOut(duration: 0.15), value: scrolled)
    }

    /// Access sits between Memory and Settings for an owner's hq agent whose host can answer it.
    private var showsAccess: Bool { access?.available == true }
    private var tabs: [(id: DetailsTab, label: String)] {
        [(.details, "Details"), (.memory, "Memory")] + (showsAccess ? [(.access, "Access")] : []) + [(.settings, "Settings")]
    }

    /// The ink Call capsule; white on night in dark. `blocked` is why it can't be used now.
    private func callButton(_ call: CallAction) -> some View {
        VStack(spacing: 6) {
            Button(action: call.start) {
                Label("Call", systemImage: "phone.fill")
                    .font(.system(size: 14, weight: .semibold))
                    .foregroundStyle(Palette.onAccent)
                    .padding(.horizontal, 26)
                    .padding(.vertical, 9)
                    .background(Palette.ink.opacity(call.blocked == nil ? 1 : 0.35), in: Capsule())
            }
            .buttonStyle(PressScale())
            .disabled(call.blocked != nil)
            if let reason = call.blocked {
                Text(reason).font(.system(size: 11.5)).foregroundStyle(Palette.muted)
            }
        }
    }

    /// The bot's avatar and name, and what runs it.
    private func identity(_ bot: Bot) -> some View {
        VStack(spacing: 4) {
            CharacterAvatar(bot: bot, size: 76)
                .padding(.bottom, 8)
            Text(bot.name)
                .kithText(.agentName)
                .foregroundStyle(Palette.ink)
                .multilineTextAlignment(.center)
            HStack(spacing: 5) {
                AgentIcon(registry: model.hello?.backends.first { $0.id == bot.backend }?.registry ?? bot.backend, size: 12)
                Text(bot.model.map { "\(model.backendName(bot.backend)) · \($0)" } ?? model.backendName(bot.backend))
            }
            .font(.system(size: 11.5))
            .foregroundStyle(Palette.muted)
            if let agent = bot.hqAgent, let kith { KithIdentityLine(agent: agent, store: kith).padding(.top, 4) }
            if let call { callButton(call).padding(.top, 12) }
        }
        .frame(maxWidth: .infinity)
        .padding(.top, 64)
    }

    /// The Details page: About, Cost, Schedules & routines, then what the package brings (kith bots).
    @ViewBuilder private var detailsPage: some View {
        if let kith, let bot {
            KithDetailsSections(store: kith, tint: AgentTint.resolve(bot.avatarColor)) {
                about
            } routines: { schedules, unreadable in
                routines(schedules, unreadable)
            }
            footer(kith)
        } else {
            VStack(alignment: .leading, spacing: 24) {
                about
                routines([], false)
            }
        }
    }

    @ViewBuilder private var about: some View {
        if let draft = Binding($draft) {
            BotSettingsForm(draft: draft, error: saveError, scrolls: false, part: .often, shown: shown, kith: kith, permissionInAccess: showsAccess)
        }
    }

    private func routines(_ schedules: [KithSchedule], _ unreadable: Bool) -> some View {
        RoutinesView(botId: botId, initialId: routineId, leave: { dismiss() }, kithSchedules: schedules,
                     schedulesUnreadable: unreadable, botName: bot?.name ?? "the bot")
    }

    /// "Updated 9:41 · pull down to refresh".
    @ViewBuilder private func footer(_ store: KithInfoStore) -> some View {
        if case .loaded(let response) = store.phase, let at = response.fetchedAt {
            Text("Updated \(Date(timeIntervalSince1970: Double(at) / 1000).formatted(date: .omitted, time: .shortened)) · pull down to refresh")
                .font(.system(size: 11.5))
                .foregroundStyle(Palette.muted)
                .frame(maxWidth: .infinity)
                .padding(.top, 16)
        }
    }

    @ViewBuilder private var page: some View {
        switch tab {
        case .details:
            detailsPage
        case .memory:
            VStack(alignment: .leading, spacing: 20) {
                // An hq agent's memory is its files; nothing is drawn until the bot is loaded, so the tab doesn't flip.
                if let bot {
                    if let agent = bot.hqAgent, !agent.isEmpty, !bot.isGroup {
                        HqMemoryCard(botId: botId, kith: kith)
                    } else {
                        MemoryCard(botId: botId, showsHarnessTab: shown.harnessMemoryTab)
                    }
                }
            }
        case .access:
            if let access, showsAccess { AccessTabView(store: access, kith: kith) } else { detailsPage }
        case .settings:
            if let draft = Binding($draft) {
                BotSettingsForm(draft: draft, error: saveError, scrolls: false, part: .rarely, shown: shown, kith: kith)
            }
        }
    }

    /// Saves a moment after the last change, like the desktop inspector.
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
                saveError = error.localizedDescription
            }
        }
    }
}

/// Pull to refresh asks kith again (`refresh: true`); other bots keep a page that doesn't pull.
private struct KithRefresh: ViewModifier {
    let store: KithInfoStore?

    @ViewBuilder func body(content: Content) -> some View {
        if let store {
            content.refreshable { await store.load(refresh: true) }
        } else {
            content
        }
    }
}

private struct IdentityBottomKey: PreferenceKey {
    static let defaultValue: CGFloat = 200
    static func reduce(value: inout CGFloat, nextValue: () -> CGFloat) { value = nextValue() }
}
