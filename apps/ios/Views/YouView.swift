import KithKit
import KithUI
import SwiftUI
import WidgetKit

/// You: what the profile picture opens. The account on top, then Usage and Routines, then the pages that already
/// exist (Computers, Keys, Extensions, Notifications, Voice, Widgets, Settings) in the order of the design. A person
/// invited to a computer sees only what is theirs: no Usage, Routines, Keys, Extensions, Voice or Widgets.
struct YouView: View {
    @Environment(AppStore.self) private var app
    @Environment(AccountStore.self) private var accounts
    @Environment(AccountSession.self) private var account
    @Environment(\.dismissModal) private var dismissModal
    @AppStorage("hiddenComputers") private var hiddenComputers = ""
    @State private var showAccount = false
    @State private var showUsage = false
    @State private var showRoutines = false
    @State private var showComputers = false
    @State private var showVoice = false
    @State private var showWidgets = false
    @State private var showSettings = false
    @State private var showKeys = false

    /// A guest (joined computers by invite only) has no usage, extensions or widgets: those are the owner's.
    private var ownerThings: Bool { accounts.computers.isEmpty || !accounts.ownedComputers.isEmpty }

    private var accountSubtitle: String {
        if account.isSignedIn { return "Signed in" }
        let count = accounts.computers.count
        return count == 0 ? "Local pairing" : "Local pairing · \(count) Mac\(count == 1 ? "" : "s")"
    }

    var body: some View {
        CardForm {
            Button { showAccount = true } label: {
                HStack(spacing: 12) {
                    ProfilePicture().frame(width: 44, height: 44).clipShape(Circle())
                    VStack(alignment: .leading, spacing: 2) {
                        Text(account.email ?? "This iPhone").font(.body.weight(.semibold)).foregroundStyle(Palette.text)
                        Text(accountSubtitle).font(.subheadline).foregroundStyle(Palette.secondary)
                    }
                    Spacer(minLength: 8)
                    Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(Palette.tertiary)
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Account")

            if ownerThings {
                CardSection {
                    row("Usage", "chart.bar") { showUsage = true }
                    row("Routines", "clock") { showRoutines = true }
                }
            }

            CardSection {
                row("Computers", "desktopcomputer", detail: ComputerProblem.healthLine(in: accounts, hidden: hiddenComputers)) { showComputers = true }
                if ownerThings, let store = app.currentStore {
                    row("Keys", "key") { showKeys = true }
                        .accessibilityHint("Service keys on \(store.hostName)")
                    row("Extensions", "puzzlepiece.extension") {
                        let id = store.computer.id
                        dismissModal()
                        Task {
                            try? await Task.sleep(for: .milliseconds(450))
                            app.marketplace = id
                        }
                    }
                }
            }

            CardSection {
                row("Notifications and display", "bell") { showSettings = true }
                if ownerThings, app.currentStore != nil {
                    row("Voice and calls", "waveform") { showVoice = true }
                    row("Widgets", "rectangle.stack") { showWidgets = true }
                }
                row("Settings", "gearshape") { showSettings = true }
            }
        }
        .page("You")
        .navigationDestination(isPresented: $showAccount) { AccountSwitcherView(pushed: true) }
        #if DEBUG
        .task {
            // `-kithDemoScreens usage` / `routines` open their page over the You page.
            try? await Task.sleep(for: .milliseconds(600))
            switch DemoScreens.name {
            case "usage": showUsage = true
            case "routines": showRoutines = true
            default: break
            }
        }
        #endif
        .navigationDestination(isPresented: $showComputers) { AccountSwitcherView(pushed: true) }
        .navigationDestination(isPresented: $showUsage) {
            YouComputerPage("Usage") { UsageView().environment($0).id($0.computer.id) }
        }
        .navigationDestination(isPresented: $showRoutines) {
            YouComputerPage("Routines") { RoutinesByAgentView(store: $0).environment($0).id($0.computer.id) }
        }
        .navigationDestination(isPresented: $showVoice) {
            if let store = app.currentStore { VoiceChatSettingsView(pushed: true).environment(store) }
        }
        .navigationDestination(isPresented: $showWidgets) { WidgetsAndActivitiesView() }
        .navigationDestination(isPresented: $showSettings) { SettingsView(pushed: true) }
        .kithSheet(isPresented: $showKeys) {
            if let store = app.currentStore { KeysView(keys: KeysStore(store: store), hostName: store.hostName) }
        }
    }

    private func row(_ title: String, _ icon: String, detail: String? = nil, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            LinkRow {
                HStack(spacing: 8) {
                    Label(title, systemImage: icon)
                    if let detail {
                        Spacer(minLength: 8)
                        Text(detail).font(.subheadline).foregroundStyle(Palette.secondary).lineLimit(1)
                    }
                }
            }
        }
        .buttonStyle(.plain)
    }
}

/// A pushed page that shows one computer's data: the last active one, or the one picked here (which also becomes the
/// computer the Usage widget shows). With several computers a badge in the header switches; the figures are always one
/// computer's, never summed.
private struct YouComputerPage<Content: View>: View {
    let title: String
    @ViewBuilder let content: (BotStore) -> Content
    @Environment(AppStore.self) private var app
    @Environment(AccountStore.self) private var accounts
    @State private var picked: ComputerID?

    init(_ title: String, @ViewBuilder content: @escaping (BotStore) -> Content) {
        self.title = title
        self.content = content
    }

    var body: some View {
        let store = picked.flatMap(accounts.store(for:)) ?? app.currentStore
        VStack(spacing: 0) {
            PageHeader(title, pushed: true) {
                if let store, accounts.ownedComputers.count > 1 {
                    DropdownMenu {
                        accounts.ownedComputers.map { computer in
                            MenuItem(computer.name, selected: computer.id == store.computer.id) {
                                picked = computer.id
                                accounts.storage.lastComputerId = computer.id
                                WidgetCenter.shared.reloadTimelines(ofKind: "KithUsage")
                            }
                        }
                    } label: {
                        ComputerBadge(store.computer, size: 28)
                    }
                    .accessibilityLabel("Computer: \(store.computer.name)")
                }
            }
            if let store {
                content(store)
            } else {
                EmptyState(title: "No computer yet", systemImage: "desktopcomputer",
                           message: "This shows up once a computer is connected.")
            }
        }
        .background(Palette.background)
        .hidesSystemNavigationBar()
    }
}
