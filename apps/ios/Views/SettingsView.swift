import KithKit
import KithUI
import SwiftUI

/// Settings for the selected account: the account row and any Mac that needs you first, then Crew, People,
/// voice, notifications and the rest. The computers themselves live in Account › Your devices.
struct SettingsView: View {
    @Environment(AppStore.self) private var app
    @Environment(AccountStore.self) private var accounts
    @Environment(\.openURL) private var openURL
    @State private var notificationsAllowed: Bool?
    @AppStorage("notificationsEnabled") private var notificationsEnabled = true
    @AppStorage(Analytics.enabledKey) private var analyticsEnabled = true
    @AppStorage(TextSizePreference.key) private var textSize = TextSizePreference.system.rawValue
    @AppStorage(AppearancePreference.key) private var appearance = AppearancePreference.system.rawValue
    @State private var pairing = false
    @State private var confirmStartOver = false
    @State private var aboutAnalytics = false
    @State private var showLicenses = false
    @AppStorage(CrewOrderMode.storageKey) private var crewOrder = CrewOrderMode.automatic.rawValue
    @AppStorage("hiddenComputers") private var hiddenComputers = ""
    @State private var widgets = false
    @State private var showAccount = false
    /// The computer whose page a problem card opens, and the one whose Voice chat page is pushed (computer IDs).
    @State private var computerPage: ComputerID?
    @State private var voicePage: ComputerID?

    var body: some View {
        // What only the owner of a computer can use: a person invited to one sees none of it.
        let online = accounts.ownedComputers.compactMap { accounts.store(for: $0.id) }.filter { $0.connection == .online }
        CardForm {
            AccountCard { showAccount = true }

            ForEach(ComputerProblem.all(in: accounts, hidden: hiddenComputers)) { problem in
                MacProblemCard(problem: problem, details: problem.hasPage ? { computerPage = problem.id } : nil,
                               reconnect: { accounts.store(for: problem.id)?.restartStream() },
                               pairAgain: { pairing = true })
            }

            CardSection("Crew", footer: crewOrder == CrewOrderMode.manual.rawValue
                        ? "Your drag order for every room, synced to your computers. Pinned rooms stay on top."
                        : "Pinned rooms first, then rooms that need you, then working, then recent. Pinned rooms can still be dragged.") {
                MenuRow("Crew order", systemImage: "person.2", selection: $crewOrder,
                        options: CrewOrderMode.allCases.map { ($0.rawValue, $0.label) })
                if accounts.computers.isEmpty || !accounts.ownedComputers.isEmpty {
                    Button { widgets = true } label: {
                        LinkRow { Label("Widgets & Live Activities", systemImage: "rectangle.stack") }
                    }
                    .buttonStyle(.plain)
                }
            }

            if let owner = online.first {
                PeopleSection(store: owner)
                // Owner only: `online` holds the computers this account owns.
                KeysSection(store: owner)
                ReplyCardsSection(store: owner)
            }

            let memberStores = accounts.computers.filter { accounts.role(for: $0.id) == .member }
                .compactMap { accounts.store(for: $0.id) }.filter { $0.connection == .online }
            if !memberStores.isEmpty {
                BotsSection(stores: memberStores)
            }

            AccountShareSlot()

            if !online.isEmpty {
                CardSection("Voice chat", footer: "Apple speech on this iPhone, or OpenAI and Gemini on your own API key, kept on the computer.") {
                    ForEach(online, id: \.computer.id) { store in
                        Button { voicePage = store.computer.id } label: {
                            LinkRow {
                                Label(online.count > 1 ? "Voice chat on \(store.hostName)" : "Voice and API keys", systemImage: "waveform")
                            }
                        }
                        .buttonStyle(.plain)
                    }
                }
            }

            // Linking their own Telegram or Discord account is for an invited person too.
            if let linkStore = online.first ?? accounts.computers.compactMap({ accounts.store(for: $0.id) }).first(where: { $0.connection == .online }) {
                LinkChatSection(store: linkStore)
            }

            CardSection("Notifications, display", footer: "Get a result summary, a request for input, or a failure notice. Default text size and appearance follow iOS Settings; Small, Large and Extra large replace it inside Kith.") {
                Toggle("Push notifications", isOn: $notificationsEnabled)
                    .toggleStyle(.kithGreen)
                    .onChange(of: notificationsEnabled) { PushRegistrar.shared.resync() }
                notificationsRow
                MenuRow("Text size", systemImage: "textformat.size", selection: $textSize,
                        options: TextSizePreference.allCases.map { ($0.rawValue, $0.label) })
                MenuRow("Appearance", systemImage: "circle.lefthalf.filled", selection: $appearance,
                        options: AppearancePreference.allCases.map { ($0.rawValue, $0.label) })
            }

            CardSection("Privacy", footer: "Which features you use, never your messages, code or files. Helps us spot bugs sooner and make Kith better for you.") {
                HStack(spacing: 4) {
                    Text("Share usage analytics").lineLimit(1).layoutPriority(1)
                    IconButton("About usage analytics", systemImage: "info.circle") { aboutAnalytics = true }
                    Toggle(isOn: Binding(get: { analyticsEnabled }, set: { Analytics.shared.setEnabled($0) })) { EmptyView() }
                        .toggleStyle(.kithGreen)
                        .accessibilityLabel("Share usage analytics")
                }
            }

            if !hiddenBots.isEmpty {
                CardSection("Hidden agents") {
                    ForEach(hiddenBots) { item in
                        let store = item.store, bot = item.bot
                        HStack {
                            CharacterAvatar(bot: bot, size: 28, animated: false)
                            VStack(alignment: .leading, spacing: 1) {
                                Text(bot.name)
                                Text(store.hostName).font(.caption).foregroundStyle(Palette.tertiary)
                            }
                            Spacer()
                            IconButton("Unhide", systemImage: "eye") {
                                withAnimation(Motion.layout) { store.setHidden(bot, false) }
                            }
                        }
                    }
                }
            }

            CardSection("About") {
                ValueRow("App version", value: Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "")
                Button("Source code") { openURL(URL(string: "https://github.com/Ying-Kai-Liao/kith")!) }
                    .buttonStyle(.plain)
                    .foregroundStyle(Palette.text)
                Button("Open-source licenses") { showLicenses = true }
                    .buttonStyle(.plain)
                    .foregroundStyle(Palette.text)
            }

            CardSection {
                Button { confirmStartOver = true } label: {
                    Text("Start over…")
                        .foregroundStyle(Palette.danger)
                        .frame(maxWidth: .infinity, alignment: .center)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            }
        }
        .navigationDestination(isPresented: $widgets) { WidgetsAndActivitiesView() }
        .navigationDestination(isPresented: $showAccount) { AccountSwitcherView(pushed: true) }
        .navigationDestination(item: $computerPage) { id in ComputerPageDestination(id: id) }
        .navigationDestination(item: $voicePage) { id in
            if let store = accounts.store(for: id) { VoiceChatSettingsView(pushed: true).environment(store) }
        }
        .kithDialog("Start over?", isPresented: $confirmStartOver,
                      message: "Signs out of every account and forgets every computer on this iPhone, then shows the welcome again. Your computers keep their bots and chats; pair again to use them.") {
            [DialogAction("Start over", destructive: true) { Task { await app.startOver() } }]
        }
        .page("Settings")
        .kithSheet(isPresented: $pairing) { PairingView(inModal: true) }
        .kithSheet(isPresented: $aboutAnalytics) { AnalyticsSheet() }
        .kithSheet(isPresented: $showLicenses) { LicensesSheet() }
        .task { notificationsAllowed = await PushRegistrar.shared.isAllowed() }
        .task { if app.openWidgets { app.openWidgets = false; widgets = true } }
        .onChange(of: app.openWidgets) { _, open in if open { app.openWidgets = false; widgets = true } }
    }


    private struct HiddenBot: Identifiable {
        let store: BotStore
        let bot: Bot
        let id: String
    }

    private var hiddenBots: [HiddenBot] {
        accounts.computers.compactMap { accounts.store(for: $0.id) }.flatMap { store in
            store.hiddenBots.map { HiddenBot(store: store, bot: $0, id: "\(store.computer.id)/\($0.id)") }
        }
    }

    private var notificationsRow: some View {
        Button {
            if notificationsAllowed != nil {
                UIApplication.shared.open(URL(string: UIApplication.openNotificationSettingsURLString)!)
            } else {
                Task { notificationsAllowed = await PushRegistrar.shared.requestAuthorization() }
            }
        } label: {
            ValueRow("Notifications", value: notificationsAllowed == true ? "On" : notificationsAllowed == false ? "Off in Settings" : "Turn on")
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
    }
}

/// The account at the top: your sky picture (or photo), who you are, how your Macs are; opens the account page.
private struct AccountCard: View {
    let open: () -> Void
    @Environment(AccountSession.self) private var account
    @Environment(AccountStore.self) private var accounts
    @AppStorage("hiddenComputers") private var hiddenComputers = ""
    @State private var changingPhoto = false

    var body: some View {
        Button(action: open) {
            HStack(spacing: 12) {
                Button { changingPhoto = true } label: {
                    if let url = account.avatarURL {
                        AccountAvatar(url: url, email: account.email, size: 44)
                    } else {
                        SkyPatch(id: account.userID ?? "you", size: 44, isYou: true)
                    }
                }
                .buttonStyle(.plain)
                .disabled(!account.isSignedIn)
                .accessibilityLabel("Change photo")
                VStack(alignment: .leading, spacing: 2) {
                    Text(title).font(.body.weight(.semibold)).foregroundStyle(Palette.text).lineLimit(1).truncationMode(.middle)
                    Text(subtitle).font(.subheadline).foregroundStyle(Palette.secondary).lineLimit(1)
                }
                Spacer(minLength: 8)
                Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(Palette.tertiary)
            }
            .padding(14)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Palette.panel, in: RoundedRectangle(cornerRadius: KithRadius.card, style: .continuous))
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .sheet(isPresented: $changingPhoto) { ProfileImagePicker() }
        .accessibilityLabel("Account")
        .accessibilityValue("\(title), \(subtitle)")
    }

    private var title: String { account.isSignedIn ? (account.email ?? "Kith account") : "Not signed in" }

    /// Signed in: how the Macs are doing. Signed out: how to start.
    private var subtitle: String {
        account.isSignedIn ? ComputerProblem.healthLine(in: accounts, hidden: hiddenComputers) : "Sign in or scan a Mac's code"
    }
}

/// A row whose value is picked from a stock menu (Crew order, Text size).
struct MenuRow<ID: Hashable>: View {
    let title: String
    let systemImage: String
    @Binding var selection: ID
    let options: [(ID, String)]

    init(_ title: String, systemImage: String, selection: Binding<ID>, options: [(ID, String)]) {
        self.title = title
        self.systemImage = systemImage
        _selection = selection
        self.options = options
    }

    var body: some View {
        DropdownMenu {
            options.map { option in
                MenuItem(option.1, selected: option.0 == selection) { selection = option.0 }
            }
        } label: {
            HStack {
                Label(title, systemImage: systemImage).foregroundStyle(Palette.text)
                Spacer(minLength: 8)
                Text(options.first { $0.0 == selection }?.1 ?? "").foregroundStyle(Palette.secondary)
                Image(systemName: "chevron.up.chevron.down").font(.caption2.weight(.semibold)).foregroundStyle(Palette.tertiary)
            }
            .contentShape(Rectangle())
        }
    }
}

/// A navigation row label with a trailing chevron.
struct LinkRow<Content: View>: View {
    @ViewBuilder let content: Content

    var body: some View {
        HStack {
            content.foregroundStyle(Palette.text)
            Spacer(minLength: 8)
            Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(Palette.tertiary)
        }
        .contentShape(Rectangle())
    }
}

/// The computer color choices: a grid of colored dots, the current one checked.
struct SwatchPanel: View {
    let selected: String?
    let pick: (String) -> Void

    var body: some View {
        LazyVGrid(columns: Array(repeating: GridItem(.fixed(36), spacing: 10), count: 6), spacing: 10) {
            ForEach(AvatarPalette.colors) { swatch in
                Button { pick(swatch.id) } label: {
                    Circle().fill(swatch.color)
                        .frame(width: 32, height: 32)
                        .overlay {
                            if selected == swatch.id {
                                Image(systemName: "checkmark").font(.caption.weight(.bold)).foregroundStyle(.white)
                            }
                        }
                }
                .buttonStyle(.plain)
                .accessibilityLabel(swatch.label)
                .accessibilityAddTraits(selected == swatch.id ? .isSelected : [])
            }
        }
        .padding(14)
    }
}

extension ConnectionRoute {
    var title: String {
        switch self {
        case .automatic: "Auto: Wi-Fi or Tailscale, then Cloudflare"
        case .cloudflareFirst: "Cloudflare, then Wi-Fi or Tailscale"
        case .directOnly: "Wi-Fi or Tailscale only, never Cloudflare"
        }
    }

    var icon: String {
        switch self {
        case .automatic: "arrow.triangle.branch"
        case .cloudflareFirst: "cloud"
        case .directOnly: "wifi"
        }
    }
}

/// The in-app text size: Default leaves iOS Dynamic Type alone; the others replace it for the whole app.
enum TextSizePreference: String, CaseIterable {
    case system, small, large, extraLarge

    static let key = "textSize"

    var label: String {
        switch self {
        case .system: "Default"
        case .small: "Small"
        case .large: "Large"
        case .extraLarge: "Extra large"
        }
    }

    /// nil means no override (iOS's own setting applies, as before this setting existed).
    var size: DynamicTypeSize? {
        switch self {
        case .system: nil
        case .small: .small
        case .large: .xLarge
        case .extraLarge: .xxxLarge
        }
    }

    init(stored: String) { self = TextSizePreference(rawValue: stored) ?? .system }
}

private struct TextSizeOverride: ViewModifier {
    @AppStorage(TextSizePreference.key) private var stored = TextSizePreference.system.rawValue

    @Environment(\.dynamicTypeSize) private var system

    /// One branch only: a conditional would give the root two view identities and reset navigation
    /// and open sheets whenever the setting changes.
    func body(content: Content) -> some View {
        content.dynamicTypeSize(TextSizePreference(stored: stored).size ?? system)
    }
}

extension View {
    /// Applies the Text size setting; put it once at the app root.
    func appTextSize() -> some View { modifier(TextSizeOverride()) }
}

/// "Link Telegram or Discord": a one-time code that ties a chat account to your Kith person.
/// Hidden when the computer's host is older than the `linkCode` method.
struct LinkChatSection: View {
    let store: BotStore
    @State private var code: LinkCode?
    @State private var busy = false
    @State private var unsupported = false
    @State private var copied = false
    @State private var error: String?

    var body: some View {
        if !unsupported {
            CardSection("Link Telegram or Discord", footer: error ?? "Chat with your agents from your own Telegram or Discord account.") {
                if let code {
                    VStack(alignment: .leading, spacing: 8) {
                        Text(code.code)
                            .font(.system(size: 34, weight: .semibold, design: .monospaced))
                            .foregroundStyle(Palette.text)
                            .textSelection(.enabled)
                        TimelineView(.periodic(from: .now, by: 30)) { context in
                            let minutes = max(1, Int((code.expiry.timeIntervalSince(context.date) / 60).rounded()))
                            Text("Send /link \(code.code) to your Kith bot in Telegram or Discord. Expires in about \(minutes) min.")
                                .font(.footnote)
                                .foregroundStyle(Palette.secondary)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                    Button(copied ? "Copied" : "Copy") {
                        UIPasteboard.general.string = "/link \(code.code)"
                        copied = true
                    }
                    .buttonStyle(.plain)
                    .foregroundStyle(Palette.text)
                }
                Button { Task { await request() } } label: {
                    Label(code == nil ? "Get a link code" : "New code", systemImage: "link")
                        .foregroundStyle(Palette.text)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .disabled(busy)
                ScheduleMessagesToggle(store: store)
            }
        }
    }

    private func request() async {
        guard !busy, let client = store.client else { return }
        busy = true
        error = nil
        copied = false
        defer { busy = false }
        do {
            code = try await client.linkCode()
        } catch let HostError.http(status, message) where status == 404 || message.localizedCaseInsensitiveContains("unknown method") {
            // An older host doesn't know the method: the row goes away rather than failing.
            unsupported = true
        } catch {
            self.error = error.localizedDescription
        }
    }
}

/// Whether a Schedule card's words also go to their Telegram or Discord chat with the agent; the card stays in the app
/// either way. Hidden until the host answers, so an older host never shows it.
struct ScheduleMessagesToggle: View {
    let store: BotStore
    @State private var on: Bool?
    @State private var error: String?

    var body: some View {
        Group {
            if let on {
                Toggle(isOn: Binding(get: { on }, set: { next in Task { await set(next) } })) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Schedule changes there too")
                        Text(error ?? "When an agent adds, changes or removes a scheduled job")
                            .font(.footnote)
                            .foregroundStyle(error == nil ? Palette.secondary : Palette.attention)
                    }
                }
                .toggleStyle(.kithGreen)
            }
        }
        .task(id: store.computer.id) {
            on = try? await store.client?.scheduleMessages()
        }
    }

    private func set(_ next: Bool) async {
        guard let client = store.client else { return }
        on = next
        error = nil
        do {
            on = try await client.scheduleMessages(on: next)
        } catch {
            on = !next
            self.error = error.localizedDescription
        }
    }
}

/// The owner's switch for reply cards: Jev sorts a finished reply that reads like a result into a card, using the
/// reply's own words (off until turned on). Hidden until the host answers, so an older host never shows it.
struct ReplyCardsSection: View {
    let store: BotStore
    @State private var on: Bool?
    @State private var error: String?

    var body: some View {
        if let on {
            CardSection("Reply cards", footer: error ?? "Lets Jev turn a reply that reads like a result into a card. The card uses the reply's own words and the reply stays one tap away.") {
                Toggle(isOn: Binding(get: { on }, set: { next in Task { await set(next) } })) {
                    Text("Result cards for replies")
                }
                .toggleStyle(.kithGreen)
            }
        } else {
            Color.clear.frame(height: 0)
                .task(id: store.computer.id) { on = try? await store.client?.replyCards() }
        }
    }

    private func set(_ next: Bool) async {
        guard let client = store.client else { return }
        on = next
        error = nil
        do {
            on = try await client.replyCards(enabled: next)
        } catch {
            on = !next
            self.error = error.localizedDescription
        }
    }
}

/// The in-app appearance: System follows iOS; Light and Dark replace it for the whole app.
enum AppearancePreference: String, CaseIterable {
    case system, light, dark

    static let key = "appearance"

    var label: String {
        switch self {
        case .system: "System"
        case .light: "Light"
        case .dark: "Dark"
        }
    }

    /// .unspecified means no override. An unknown stored value (from a newer version) falls back to System.
    var interfaceStyle: UIUserInterfaceStyle {
        switch self {
        case .system: .unspecified
        case .light: .light
        case .dark: .dark
        }
    }

    init(stored: String) { self = AppearancePreference(rawValue: stored) ?? .system }
}

private struct AppearanceOverride: ViewModifier {
    @AppStorage(AppearancePreference.key) private var stored = AppearancePreference.system.rawValue

    // Set on the UIWindows, not with .preferredColorScheme: SwiftUI's modifier often fails to go back to
    // System after Light/Dark until relaunch, and it is flaky inside presentations. A window-level style
    // is inherited by every sheet and cover, and .unspecified hands control back to the phone.
    func body(content: Content) -> some View {
        content
            .onAppear(perform: apply)
            .onChange(of: stored) { apply() }
            .onReceive(NotificationCenter.default.publisher(for: UIWindow.didBecomeKeyNotification)) { _ in apply() }
            .onReceive(NotificationCenter.default.publisher(for: UIScene.didActivateNotification)) { _ in apply() }
    }

    private func apply() {
        let style = AppearancePreference(stored: stored).interfaceStyle
        for case let scene as UIWindowScene in UIApplication.shared.connectedScenes {
            for window in scene.windows where window.overrideUserInterfaceStyle != style {
                window.overrideUserInterfaceStyle = style
            }
        }
    }
}

extension View {
    /// Applies the Appearance setting; put it once at the app root.
    func appAppearance() -> some View { modifier(AppearanceOverride()) }
}
