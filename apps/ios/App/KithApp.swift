import KithKit
import KithUI
import SwiftUI
import Observation
import UserNotifications
import WidgetKit

@main
struct KithApp: App {
    @UIApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @State private var app = AppStore.shared
    @Environment(\.scenePhase) private var scenePhase
    @AppStorage("onboardingCompleted") private var onboardingCompleted = false
    /// Cold launch with a computer to reach: the splash covers its connect and catch-up.
    @State private var launching = !AppStore.shared.accounts.computers.isEmpty && !DemoScreens.active
    init() {
        // A tapped widget bot arrives as an intent, not a URL.
        OpenBotIntent.open = { AppStore.shared.open($0) }
        KithUIFonts.register()
    }

    var body: some Scene {
        WindowGroup {
            RootView()
                .id("\(app.contextID)#\(app.generation)")
                .environment(app)
                .environment(app.accounts)
                .environment(app.account)
                .tint(Palette.accent)
                .appTextSize()
                .appAppearance()
                // No scroll indicators anywhere in the app.
                .scrollIndicators(.never)
                .kithSheet(isPresented: Binding(
                    get: { (onboardingCompleted || app.account.isSignedIn) && app.account.showSwitcher },
                    set: { app.account.showSwitcher = $0 }
                )) {
                    // Holds the pushes inside the sheet (Computers & settings, Widgets); no bar shows.
                    NavigationStack { AccountSwitcherView() }
                        .environment(app)
                        .environment(app.accounts)
                        .environment(app.account)
                        .tint(Palette.accent)
                }
                .kithSheet(item: Binding(get: { app.inviteJoin }, set: { if $0 == nil { app.closeInviteJoin() } })) { join in
                    // Above RootView, which is rebuilt when signing in switches the account's stores.
                    InviteJoinView(model: join)
                        .environment(app)
                        .environment(app.accounts)
                        .environment(app.account)
                        .tint(Palette.accent)
                }
                .onChange(of: app.account.userID, initial: true) { _, userID in
                    app.switchAccount(to: userID)
                }
                .overlay {
                    if launching {
                        LaunchSplash(stores: app.accounts.computers.compactMap { app.accounts.store(for: $0.id) }) {
                            withAnimation(.easeOut(duration: 0.35)) { launching = false }
                        }
                    }
                }
                .onOpenURL { url in app.open(url) }
                .onChange(of: onboardingCompleted) { _, done in if done { Analytics.shared.capture(.onboardingCompleted) } }
                .onChange(of: scenePhase, initial: true) { _, phase in
                    switch phase {
                    case .active: Analytics.shared.becameActive()
                    case .background: Analytics.shared.enteredBackground()
                    default: break
                    }
                    // Only leaving for the background disconnects; `.inactive` (Control Center,
                    // app switcher, system prompts) comes and goes too often to drop the link.
                    app.accounts.setActive(phase != .background && !DemoScreens.active)
                    // Back in the app: a computer may have joined the account meanwhile.
                    if phase == .active {
                        Task { await app.accounts.refreshCloud() }
                        app.deliverShares()
                    }
                }
                #if DEBUG
                .modifier(DebugHooks(app: app))
                #endif
        }
    }
}

#if DEBUG
/// The screenshot and simulator hooks (galleries, pairing, demo screens), kept out of `KithApp.body` so its
/// modifier chain stays quick to type-check.
private struct DebugHooks: ViewModifier {
    let app: AppStore
    /// The agent sheet demo shots (`-kithDemoScreens agent`, `agent-memory`, `agent-settings`).
    @State private var agentScreen: String?
    /// The share sheet demo shots (`share-sheet`, `share-link`).
    @State private var shareScreen: String?
    /// The bot guest demo shots (`bots-people`, `bots-edit`, `bots-invite`, `bots-result`, `bots-detail`, `bots-remove`).
    @State private var botScreen: String?

    func body(content: Content) -> some View {
        content
        .fullScreenCover(isPresented: Binding(get: { agentScreen != nil }, set: { if !$0 { agentScreen = nil } })) {
            if let screen = agentScreen, let computer = app.accounts.computers.first, let store = app.accounts.store(for: computer.id) {
                if screen == "call" || screen == "incoming-call" {
                    DemoCallScreen(store: store, incoming: screen == "incoming-call")
                } else {
                    DemoAgentSheet(store: store, screen: screen)
                }
            }
        }
        .kithSheet(isPresented: Binding(get: { shareScreen != nil }, set: { if !$0 { shareScreen = nil } })) {
            if let screen = shareScreen, let computer = app.accounts.computers.first, let store = app.accounts.store(for: computer.id) {
                ShareAgentSheet(store: store, agent: "ada", name: "Ada", people: PeopleStore.demo(),
                                created: screen == "share-link" ? DemoScreens.demoInvite : nil)
            }
        }
        .kithSheet(isPresented: Binding(get: { botScreen != nil }, set: { if !$0 { botScreen = nil } })) {
            if let screen = botScreen, let computer = app.accounts.computers.first, let store = app.accounts.store(for: computer.id) {
                DemoBotScreen(store: store, screen: screen).environment(app.accounts)
            }
        }
        .overlay {
            // Design token gallery for screenshots: launch with -kithTokenGallery.
            if ProcessInfo.processInfo.arguments.contains("-kithTokenGallery") { KithTokenGallery() }
            if ProcessInfo.processInfo.arguments.contains("-kithComponentGallery") { KithComponentGallery() }
            if let mode = ProfileImagePickerPreview.mode { ProfileImagePickerPreview.view(mode).background(Palette.background) }
            // First-run and pairing shots sit over the demo account's home: `-kithDemoScreens pairing-<welcome|scan|install|success|error>`.
            if let screen = DemoScreens.name, screen.hasPrefix("pairing-") {
                Group {
                    if screen == "pairing-welcome" { WelcomeView {} } else { PairingView(onSkip: {}) { BackButton {} } }
                }
                .environment(app)
                .environment(app.account)
                .background(Palette.background)
            }
        }
        .task {
            // Simulator/UI-test pairing: SIMCTL_CHILD_KITH_PAIR_URL=kith://pair?...
            if let s = ProcessInfo.processInfo.environment["KITH_PAIR_URL"], let p = Pairing(string: s) {
                _ = try? await app.pair(p)
            }
            if ProcessInfo.processInfo.environment["KITH_OPEN_USAGE"] != nil { app.showUsage = true }
            // Screenshots: Settings, opened on the Widgets & Live Activities page.
            if ProcessInfo.processInfo.environment["KITH_STATE_SURFACE"] != nil { app.showSettings = true; app.openWidgets = true }
            if ProcessInfo.processInfo.environment["KITH_OPEN_SETTINGS"] != nil { app.showSettings = true }
            if ProcessInfo.processInfo.environment["KITH_OPEN_COMPUTERS"] != nil { app.showComputers = true }
            // Screenshots: KITH_OPEN_URL=kith://bot/<botId>?scope=local&computer=<id>, without the system prompt.
            if let s = ProcessInfo.processInfo.environment["KITH_OPEN_URL"], let url = URL(string: s) {
                try? await Task.sleep(for: .seconds(1))
                app.open(url)
            }
            if let screen = DemoScreens.name {
                // One case per demo screen: later redesign PRs add theirs here.
                try? await Task.sleep(for: .seconds(1))
                switch screen {
                case "settings": app.showSettings = true
                case "computers", "account", "account-devices": app.showComputers = true
                case "problem": app.showSettings = true
                case "widgets":
                    // The page is pushed inside the Settings sheet, so wait for the sheet and its stack to be up.
                    app.showSettings = true
                    try? await Task.sleep(for: .seconds(1.5))
                    app.openWidgets = true
                case "chat", "mermaid":
                    if let computer = app.accounts.computers.first {
                        app.accounts.selection = BotReference(accountId: nil, computerId: computer.id, botId: DemoScreens.chatBotId)
                    }
                case "botguest-chat", "botguest-thread", "botguest-chat-other", "botguest-thread-other":
                    if let computer = app.accounts.computers.first {
                        app.accounts.selection = BotReference(accountId: nil, computerId: computer.id, botId: DemoScreens.botGuestChatId)
                        if screen.hasPrefix("botguest-thread") {
                            app.accounts.store(for: computer.id)?.pendingThread = PendingThread(chat: DemoScreens.botGuestChatId, root: DemoScreens.botGuestRoot)
                        }
                    }
                case "botguest-detail": botScreen = screen
                case "keys-access", "keys-key", "keys-replace", "keys-states":
                    if let computer = app.accounts.computers.first {
                        app.accounts.selection = BotReference(accountId: nil, computerId: computer.id, botId: "ada")
                    }
                case "keys-list": app.showSettings = true
                case "share-chat":
                    if let computer = app.accounts.computers.first {
                        app.accounts.selection = BotReference(accountId: nil, computerId: computer.id, botId: "ada")
                    }
                case "share-sheet", "share-link": shareScreen = screen
                case "bots-share": shareScreen = "share-sheet"
                case "bots-settings": app.showSettings = true
                case "bots-people", "bots-edit", "bots-invite", "bots-result", "bots-detail", "bots-remove": botScreen = screen
                case "agent", "agent-memory", "agent-settings", "call", "incoming-call": agentScreen = screen
                default: break // "crew", "crew-typing" and "board" (BotListView opens its Board tab) are the root
                }
            }
            await LiveActivities.shared.previewIfRequested()
            if ProcessInfo.processInfo.environment["KITH_OPEN_SCREEN"] != nil {
                try? await Task.sleep(for: .seconds(2))
                app.currentStore?.screenRequest = ScreenRequest()
            }
        }
    }
}
#endif

/// One `AccountStore` per account context: switching accounts retires the old one so
/// in-flight work can't land on another user's screen or storage.
@MainActor
@Observable
final class AppStore {
    static let shared = AppStore()
    let account: AccountSession
    private(set) var accounts: AccountStore
    /// The signed-in account's cloud; nil when signed out or the build has no cloud.
    private(set) var cloud: CloudClient?
    private(set) var contextID: String
    /// Bumped by Start over so the whole UI is rebuilt, even though the local context keeps its id.
    private(set) var generation = 0
    /// The Computers sheet (profile menu, `kith://computers`).
    var showComputers = false
    /// The Settings sheet (profile menu).
    var showSettings = false
    /// Settings opens on its Widgets & Live Activities page (a screenshot hook).
    var openWidgets = false
    /// The usage limits sheet (profile menu, `kith://usage`).
    var showUsage = false
    /// The computer whose marketplace is open.
    var marketplace: ComputerID?
    /// A guest's permission request opened from its push, shown as a sheet on the computer it belongs to.
    var guestPrompt: BotReference?
    /// A snoozed call's notification was tapped: the agent whose call to open.
    var callBack: BotReference?
    /// An invite link being joined, shown as a sheet.
    private(set) var inviteJoin: InviteJoinModel?

    private init() {
        let session = AccountSession()
        let storage = SharedStore.Context(accountID: session.userID)
        SharedStore.activeAccountID = session.userID
        account = session
        Analytics.shared.userID = { [session] in session.userID }
        CrashReports.shared.start()
        contextID = storage.id
        #if DEBUG
        // Screenshots in CI have no host: `-kithDemoScreens <name>` swaps in a fixed account (see DemoScreens).
        if DemoScreens.active {
            (accounts, cloud) = (AccountStore.demo(storage: storage), nil)
            return
        }
        #endif
        (accounts, cloud) = Self.makeAccounts(storage, session: session)
    }

    var storage: SharedStore.Context { accounts.storage }

    /// The computer the Usage tab, widgets and "open screen" links act on: the last active one.
    var currentStore: BotStore? {
        // Usage, plugins and the screen are the owner's: a computer joined by invite is never the one they act on.
        let owned = Set(accounts.ownedComputers.map(\.id))
        let last = accounts.storage.lastComputerId.flatMap { owned.contains($0) ? $0 : nil }
        return last.flatMap(accounts.store(for:))
            ?? accounts.ownedComputers.first.flatMap { accounts.store(for: $0.id) }
    }

    func switchAccount(to userID: String?) {
        let storage = SharedStore.Context(accountID: userID)
        guard storage.id != contextID else { return }
        accounts.retire()
        PushRegistrar.shared.deactivate()
        LiveActivities.shared.endAll()
        SharedStore.activeAccountID = userID
        storage.bots = []
        contextID = storage.id
        showComputers = false
        showSettings = false
        showUsage = false
        marketplace = nil
        guestPrompt = nil
        (accounts, cloud) = Self.makeAccounts(storage, session: account)
        BotsWidgetFeed.reset()
        WidgetCenter.shared.reloadAllTimelines()
    }

    /// Signing out forgets this account on this iPhone: its device keys, computers and caches (spec §3.2).
    func signOut() async {
        guard let userID = account.userID else { return }
        // Sent first so it still counts toward the account.
        Analytics.shared.capture(.signedOut)
        await account.signOut()
        guard account.userID != userID, !account.accounts.contains(where: { $0.id == userID }) else { return }
        // Retire its stores first: a retiring store saves its cache one last time.
        switchAccount(to: account.userID)
        SharedStore.Context(accountID: userID).erase()
    }

    /// Settings → Start over: signs out of every account and forgets every computer, key and cache
    /// on this iPhone, then shows the welcome again. The computers keep their bots and chats.
    func startOver() async {
        let userIDs = account.accounts.map(\.id)
        await account.signOutAll()
        accounts.retire()
        PushRegistrar.shared.deactivate()
        LiveActivities.shared.endAll()
        for id in userIDs { SharedStore.Context(accountID: id).erase() }
        let storage = SharedStore.Context(accountID: nil)
        storage.erase()
        storage.bots = []
        SharedStore.activeAccountID = nil
        UserDefaults.standard.set(false, forKey: "onboardingCompleted")
        showComputers = false
        showSettings = false
        showUsage = false
        account.showSwitcher = false
        marketplace = nil
        contextID = storage.id
        (accounts, cloud) = Self.makeAccounts(storage, session: account)
        generation += 1
        BotsWidgetFeed.reset()
        WidgetCenter.shared.reloadAllTimelines()
    }

    func pair(_ pairing: Pairing) async throws -> Computer {
        let computer = try await accounts.pair(pairing, deviceName: UIDevice.current.name, platform: "ios")
        Analytics.shared.capture(.computerPaired, ["via": "qr", "computer": Analytics.computerTag(computer.id)])
        return computer
    }

    func open(_ link: URL) {
        let url = LegacyCompat.canonical(link).url
        if url.scheme == Bundle.main.bundleIdentifier {
            Task { await account.handle(url) }
            return
        }
        if InviteLink.isPageLink(url) {
            // Only `/i/...` paths are ours; any other https link stays ignored.
            guard let invite = InviteLink(url, scheme: AppEnvironment.current.urlScheme) else {
                accounts.lastError = "That invite link isn't valid. Ask for a new one."
                return
            }
            joinInvite(invite)
            return
        }
        guard let scheme = url.scheme, AppEnvironment.current.accepts(scheme: scheme) else { return }
        switch url.host() {
        case "invite":
            guard let invite = InviteLink(url, scheme: scheme) else {
                accounts.lastError = "That invite link isn't valid. Ask for a new one."
                return
            }
            joinInvite(invite)
        case "pair":
            // Scanned with the Camera app: pair right away.
            let accounts = accounts
            Task {
                do { _ = try await pair(try Pairing.parse(url)) } catch { accounts.lastError = error.localizedDescription }
            }
        case "bot":
            guard let ref = storage.reference(from: url), accounts.store(for: ref.computerId) != nil else { return }
            openBot(ref)
        case "plugins":
            accounts.selection = nil
            marketplace = currentStore?.computer.id
        case "usage":
            accounts.selection = nil
            showUsage = true
        case "computers":
            accounts.selection = nil
            showComputers = true
        case "screen":
            currentStore?.screenRequest = ScreenRequest()
        default:
            break
        }
    }

    /// Opens a bot's chat. An agent's card lane, or a chat the account has no bot for on a computer it owns (a guest's),
    /// is not a chat: its push is for the request waiting there, which shows as a card.
    func openBot(_ ref: BotReference) {
        if accounts.opensGuestPrompt(ref) {
            accounts.selection = nil
            guestPrompt = ref
        } else {
            accounts.selection = ref
        }
    }

    /// Sends what the share sheet queued (ShareExtension) into the chosen chats, then opens the last one. A share for
    /// a computer this account no longer has can't go anywhere and is dropped with the expired ones.
    func deliverShares() {
        let context = storage
        var done = Set<String>()
        var last: BotReference?
        for share in context.pendingShares {
            if share.isExpired(at: .now) || share.target.accountId != context.accountID {
                done.insert(share.id)
            } else if let store = accounts.store(for: share.target.computerId) {
                store.send(share.text, to: share.target.botId)
                done.insert(share.id)
                last = share.target
            } else if !accounts.computers.contains(where: { $0.id == share.target.computerId }) {
                done.insert(share.id)
            }
        }
        context.clearShares(ids: done)
        if let last { openBot(last) }
    }

    /// The same link opened twice shows the join that is already running.
    private func joinInvite(_ link: InviteLink) {
        if let current = inviteJoin, current.link == link {
            current.start()
            return
        }
        inviteJoin?.cancel()
        let model = InviteJoinModel(link: link, app: self)
        inviteJoin = model
    }

    /// Closing the sheet keeps a waiting join going on its own: the token is kept, and connecting proves it.
    func closeInviteJoin() {
        inviteJoin?.cancel()
        inviteJoin = nil
    }

    private static func makeAccounts(_ storage: SharedStore.Context, session: AccountSession) -> (AccountStore, CloudClient?) {
        let identity = storage.accountID == nil ? nil : try? DeviceIdentity.load(context: storage)
        let cloud = session.cloudClient(for: storage.accountID, identity: identity)
        let accounts = AccountStore(storage: storage, cloud: cloud)
        accounts.onConnected = { store in
            PushRegistrar.shared.syncDevice(with: store)
            store.onUsageChanged = { _, _ in WidgetCenter.shared.reloadTimelines(ofKind: "KithUsage") }
        }
        accounts.onBotUpdated = { ref, bot in LiveActivities.shared.update(ref, bot: bot) }
        accounts.onRosterChanged = { roster in
            BotsWidgetFeed.update(roster, storage: storage)
            if roster.isEmpty {
                LiveActivities.shared.endAll()
                WidgetCenter.shared.reloadAllTimelines()
            }
        }
        accounts.onSent = { [weak accounts] ref, bot, progress in
            guard let store = accounts?.store(for: ref.computerId) else { return }
            LiveActivities.shared.sent(ref, bot: bot, progress: progress, store: store)
        }
        if let cloud {
            // Signed in: make this iPhone known to the account, then list its computers.
            Task { [weak accounts] in
                do {
                    try await cloud.registerDevice(name: UIDevice.current.name, platform: .ios)
                } catch let error as CloudError where !error.isTransient {
                    accounts?.lastError = error.localizedDescription
                } catch {
                    // Transient (network, busy cloud, token not ready): the next launch registers again.
                }
                await accounts?.refreshCloud()
            }
        }
        return (accounts, cloud)
    }

    /// Takes back this iPhone's account access to a computer (its grant), and forgets the computer here.
    /// Also the way out when an earlier install was approved but this one never checked the code.
    func revokeAccess(_ id: ComputerID) async {
        guard let cloud else { return }
        let accounts = accounts
        do {
            let identity = try DeviceIdentity.load(context: accounts.storage)
            if let device = try await cloud.devices().first(where: { $0.deviceKey == identity.publicKey }) {
                for grant in try await cloud.grants(id) where grant.deviceId == device.deviceId {
                    try await cloud.revokeGrant(id, grantId: grant.grantId)
                }
            }
            accounts.forget(id)
            await accounts.refreshCloud()
        } catch {
            accounts.lastError = error.localizedDescription
        }
    }
}

final class AppDelegate: NSObject, UIApplicationDelegate, UNUserNotificationCenterDelegate {
    func application(_ application: UIApplication, didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil) -> Bool {
        UNUserNotificationCenter.current().delegate = self
        let open = UNNotificationAction(identifier: "openConversation", title: "Open conversation", options: [.foreground])
        let review = UNNotificationAction(identifier: "reviewRequest", title: "Review request", options: [.foreground])
        UNUserNotificationCenter.current().setNotificationCategories([
            UNNotificationCategory(identifier: "done", actions: [open], intentIdentifiers: []),
            UNNotificationCategory(identifier: "needsInput", actions: [review], intentIdentifiers: []),
            UNNotificationCategory(identifier: "failed", actions: [open], intentIdentifiers: []),
        ])
        #if DEBUG
        PushRegistrar.shared.verifyDeliveryIfRequested()
        #endif
        return true
    }

    /// Portrait, except while the screen viewer is open.
    func application(_ application: UIApplication, supportedInterfaceOrientationsFor window: UIWindow?) -> UIInterfaceOrientationMask {
        OrientationLock.mask
    }

    func application(_ application: UIApplication, didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        Task { @MainActor in PushRegistrar.shared.didRegister(token: deviceToken) }
    }

    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter, didReceive response: UNNotificationResponse,
                                          withCompletionHandler completionHandler: @escaping @Sendable () -> Void) {
        let info = response.notification.request.content.userInfo
        let ctx = info["ctx"] as? String
        let computerId = info["computerId"] as? String
        let botId = info["botId"] as? String
        let callsBack = CallSnooze.isCallBack(info)
        // UIKit saves application state from this completion. The async delegate
        // bridge can complete off the main thread even after MainActor.run.
        Task { @MainActor in
            defer { completionHandler() }
            guard let ctx, let computerId, let botId else { return }
            // Only open it in the account it was sent to, on a computer that account still has.
            let accounts = AppStore.shared.accounts
            guard ctx == accounts.storage.id, accounts.store(for: computerId) != nil else { return }
            let ref = BotReference(accountId: accounts.accountId, computerId: computerId, botId: botId)
            if callsBack { AppStore.shared.callBack = ref } else { AppStore.shared.openBot(ref) }
        }
    }

    nonisolated func userNotificationCenter(_ center: UNUserNotificationCenter, willPresent notification: UNNotification) async -> UNNotificationPresentationOptions {
        // The chat already shows it live.
        []
    }
}

/// Keeps the Bots widget's snapshot current; asks WidgetKit to redraw only when
/// something the widget shows changes (who is working / needs you / unread).
@MainActor
enum BotsWidgetFeed {
    private struct Snapshot: Equatable {
        let computerId: ComputerID
        let id: String
        let name: String
        let shape: String
        let color: String
        let source: String
        let status: String
        let activity: String
        let message: String?
        let lastAt: Int64

        init(_ bot: Bot, computerId: ComputerID) {
            self.computerId = computerId
            id = bot.id; name = bot.name; shape = bot.avatarShape; color = bot.avatarColor; source = bot.avatarSource
            status = bot.status; activity = bot.activity; message = bot.lastMessage; lastAt = bot.lastAt
        }
    }
    private static var shown: [Snapshot]?
    private static var scope: String?

    static func reset() { shown = nil; scope = nil }

    static func update(_ roster: [RosterItem], storage: SharedStore.Context) {
        guard storage.id == SharedStore.activeContext.id else { return }
        storage.bots = roster.map { BotSnapshot(computerId: $0.ref.computerId, bot: $0.bot) }
        let currentScope = storage.id
        let signature = roster.filter { !$0.bot.hidden }.map { Snapshot($0.bot, computerId: $0.ref.computerId) }
        guard signature != shown || scope != currentScope else { return }
        shown = signature
        scope = currentScope
        WidgetCenter.shared.reloadTimelines(ofKind: "KithBots")
        WidgetCenter.shared.reloadTimelines(ofKind: "KithTeam")
        ControlCenter.shared.reloadControls(ofKind: "KithLeadBot")
    }
}
