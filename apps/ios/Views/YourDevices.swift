import KithKit
import KithUI
import SwiftUI

/// Account › Your devices: every computer this iPhone can use or ask for (paired here and in the account,
/// merged by computer ID), then the way to add one. It owns the computer page, the quick actions' destinations
/// and the forget, revoke and remove-copy dialogs, so the screen around it only lays it out.
struct YourDevicesSection: View {
    @Environment(AppStore.self) private var app
    @Environment(AccountStore.self) private var accounts
    @Environment(AccountSession.self) private var account
    @Environment(\.dismissModal) private var dismissModal
    @AppStorage("hiddenComputers") private var hiddenComputers = ""
    @State private var pairing = false
    @State private var confirmForget: Computer?
    @State private var confirmRevoke: CloudComputer?
    @State private var access: AccessTarget?
    @State private var confirmRemoveCopy: CloudComputer?
    @State private var newChat: VoiceTarget?
    /// The computer whose page is pushed (computer ID), and the one whose Voice chat page is.
    @State private var computerPage: ComputerID?
    @State private var voicePage: ComputerID?

    var body: some View {
        let selection = ComputerSelection(all: accounts.computers.map(\.id), hidden: hiddenComputers)
        CardSection("Your devices", footer: account.isSignedIn
                    ? "Computers in your account need your OK on the computer before this iPhone can use them."
                    : "Each bot runs on its own computer. Sign in to see the computers in your account.") {
            ForEach(accounts.computers) { computer in
                if let store = accounts.store(for: computer.id), accounts.role(for: computer.id) == .member {
                    let shown = Binding(get: { selection.shown.contains(computer.id) },
                                        set: { _ in withAnimation(Motion.layout) { hiddenComputers = selection.toggling(computer.id) } })
                    MemberComputerRow(store: store, shown: shown, newChat: { newChat = VoiceTarget(store: store) }, remove: { confirmForget = computer })
                } else if let store = accounts.store(for: computer.id) {
                    ComputerRow(store: store, hidden: !selection.shown.contains(computer.id), open: { computerPage = computer.id },
                                openScreen: { openScreen(store) },
                                openMarketplace: { closeSheets { app.marketplace = store.computer.id } },
                                openVoice: { voicePage = computer.id },
                                revoke: cloudComputer(computer.id).flatMap { c in c.access == .granted ? { confirmRevoke = c } : nil },
                                remove: { confirmForget = computer })
                }
            }
            ForEach(accountOnly) { computer in
                if accounts.isOlderCopy(computer) {
                    OlderCopyRow(computer: computer) { confirmRemoveCopy = computer }
                } else {
                    AccountComputerRow(computer: computer, ticket: accounts.pendingAccess[computer.computerId],
                                       ask: { access = AccessTarget(computer: computer) },
                                       revoke: { confirmRevoke = computer })
                }
            }
            Button { pairing = true } label: {
                Label(account.isSignedIn ? "Add a Mac" : "Scan a Mac's code",
                      systemImage: account.isSignedIn ? "plus" : "qrcode.viewfinder")
                    .foregroundStyle(Palette.accent)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
        }
        .navigationDestination(item: $computerPage) { id in ComputerPageDestination(id: id) }
        .navigationDestination(item: $voicePage) { id in
            if let store = accounts.store(for: id) { VoiceChatSettingsView(pushed: true).environment(store) }
        }
        .kithSheet(isPresented: $pairing) { PairingView(inModal: true) }
        .kithSheet(item: $newChat) { target in NewChatSheet(store: target.store).environment(accounts) }
        .kithSheet(item: $access) { target in
            AccessRequestView(computer: target.computer, pending: accounts.pendingAccess[target.id] != nil)
        }
        .kithDialog("Remove the older \(confirmRemoveCopy?.name ?? "computer")?", isPresented: Binding(
            get: { confirmRemoveCopy != nil }, set: { if !$0 { confirmRemoveCopy = nil } }
        ), message: "It's this computer under an earlier identity and can't be reached anymore. Removing it only cleans up your account.") {
            guard let copy = confirmRemoveCopy else { return [] }
            return [DialogAction("Remove", destructive: true) { Task { await accounts.removeFromAccount(copy.computerId) } }]
        }
        .kithDialog("Remove \(confirmForget?.name ?? "computer")?",
                      isPresented: Binding(get: { confirmForget != nil }, set: { if !$0 { confirmForget = nil } }),
                      message: "Its bots and conversations stay on that computer. You can pair again any time.") {
            [DialogAction("Remove", destructive: true) {
                if let c = confirmForget { accounts.forget(c.id) }
            }]
        }
        .kithDialog("Revoke this iPhone's access to \(confirmRevoke?.name ?? "the computer")?",
                      isPresented: Binding(get: { confirmRevoke != nil }, set: { if !$0 { confirmRevoke = nil } }),
                      message: "You can ask for access again; the computer will show a new code to confirm.") {
            [DialogAction("Revoke", destructive: true) {
                if let c = confirmRevoke { Task { await app.revokeAccess(c.computerId) } }
            }]
        }
    }

    private struct VoiceTarget: Identifiable {
        let store: BotStore
        let id: String

        @MainActor init(store: BotStore) {
            self.store = store
            id = store.computer.id
        }
    }

    private struct AccessTarget: Identifiable {
        let computer: CloudComputer
        var id: ComputerID { computer.computerId }
    }

    private func cloudComputer(_ id: ComputerID) -> CloudComputer? {
        accounts.cloudComputers.first { $0.computerId == id }
    }

    /// Account computers this iPhone isn't set up for yet.
    private var accountOnly: [CloudComputer] {
        accounts.cloudComputers.filter { c in !accounts.computers.contains { $0.id == c.computerId } }
    }

    /// This screen is a sheet of its own or inside Settings; the marketplace and screen open over the app,
    /// once that sheet has slid away.
    private func closeSheets(then open: @escaping @MainActor () -> Void) {
        dismissModal()
        Task {
            try? await Task.sleep(for: .milliseconds(450))
            open()
        }
    }

    private func openScreen(_ store: BotStore) {
        closeSheets { store.screenRequest = ScreenRequest() }
    }
}

/// One computer's page, pushed from a device row or from a problem card's Details.
struct ComputerPageDestination: View {
    let id: ComputerID
    @Environment(AppStore.self) private var app
    @Environment(AccountStore.self) private var accounts
    @Environment(\.dismissModal) private var dismissModal

    var body: some View {
        if let store = accounts.store(for: id) {
            let cloud = accounts.cloudComputers.first { $0.computerId == id }
            ComputerPage(store: store, revokeTarget: cloud.flatMap { $0.access == .granted ? $0 : nil },
                         openScreen: { closeSheets { store.screenRequest = ScreenRequest() } },
                         openMarketplace: { closeSheets { app.marketplace = store.computer.id } })
        }
    }

    private func closeSheets(then open: @escaping @MainActor () -> Void) {
        dismissModal()
        Task {
            try? await Task.sleep(for: .milliseconds(450))
            open()
        }
    }
}

/// A computer this account joined by invite: its name and role, a way to start a chat, and nothing of the owner's.
private struct MemberComputerRow: View {
    let store: BotStore
    @Binding var shown: Bool
    let newChat: () -> Void
    let remove: () -> Void
    @Environment(AccountStore.self) private var accounts

    var body: some View {
        HStack(spacing: 12) {
            ComputerTile(store.computer)
            VStack(alignment: .leading, spacing: 2) {
                Text("\(store.hostName) · \(accounts.access(for: store.computer.id).title)")
                    .font(.body.weight(.semibold)).foregroundStyle(Palette.text).lineLimit(1)
                Text(store.connection == .online ? "Online" : "Not connected")
                    .font(.subheadline).foregroundStyle(Palette.secondary)
            }
            Spacer(minLength: 8)
            if store.connection == .online { IconButton("New chat", systemImage: "square.and.pencil", action: newChat) }
            IconButton("Remove", systemImage: "trash", action: remove)
            ShowInCrewSwitch(name: store.hostName, isOn: $shown)
        }
    }
}

/// The switch at the end of a computer's row: whether its rooms show in Crew on this iPhone.
struct ShowInCrewSwitch: View {
    let name: String
    @Binding var isOn: Bool

    var body: some View {
        Toggle(isOn: $isOn) { EmptyView() }
            .toggleStyle(.kithGreen)
            .labelsHidden()
            .accessibilityLabel("Show \(name) in Crew")
    }
}

/// A computer this iPhone can use: tap for its page, long-press for the quick actions. Whether it shows in
/// Crew is switched on its page; here a hidden one just says so.
private struct ComputerRow: View {
    let store: BotStore
    let hidden: Bool
    let open: () -> Void
    let openScreen: () -> Void
    /// Agents, connectors and skills installed on this computer.
    let openMarketplace: () -> Void
    let openVoice: () -> Void
    /// Set when this iPhone's access to the computer can be revoked.
    let revoke: (() -> Void)?
    let remove: () -> Void
    @Environment(AccountStore.self) private var accounts
    @State private var coloring = false

    var body: some View {
        Button(action: open) {
            HStack(spacing: 12) {
                ComputerTile(store.computer)
                VStack(alignment: .leading, spacing: 2) {
                    Text(store.hostName).font(.body.weight(.semibold)).foregroundStyle(Palette.text).lineLimit(1)
                    subtitle
                }
                Spacer(minLength: 8)
                if hidden { Text("Hidden").font(.subheadline).foregroundStyle(Palette.secondary) }
                Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(Palette.tertiary)
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .padding(.vertical, 2)
        .contextActions {
            var items = [MenuItem("Colour", icon: "paintpalette") { coloring = true }]
            if !stale { items.append(MenuItem("Route", detail: routeDetail, icon: "wifi", action: open)) }
            if store.connection == .online {
                items.append(MenuItem("Extensions", icon: "square.grid.2x2", action: openMarketplace))
                items.append(MenuItem("Voice chat", icon: "waveform", action: openVoice))
                if store.screen != nil { items.append(MenuItem("Show screen", icon: "display", action: openScreen)) }
                if accounts.role(for: store.computer.id) != .member { items.append(MenuItem("Backups", icon: "archivebox", action: open)) }
            }
            if let revoke {
                items.append(MenuItem("Revoke this iPhone's access", icon: "lock.slash", destructive: true, divider: true, action: revoke))
            }
            items.append(MenuItem("Remove", icon: "trash", destructive: true, divider: revoke == nil, action: remove))
            return items
        }
        .kithOverlay(isPresented: $coloring) { close in
            ZStack {
                Color.black.opacity(0.35).ignoresSafeArea().onTapGesture(perform: close)
                SwatchPanel(selected: store.computer.color) { id in
                    close()
                    accounts.setColor(store.computer.id, id)
                }
                .background(Palette.bubbleAgent, in: RoundedRectangle(cornerRadius: 22, style: .continuous))
                .shadow(color: .black.opacity(0.25), radius: 24, y: 10)
            }
            .accessibilityAddTraits(.isModal)
        }
    }

    private var stale: Bool { accounts.isStale(store.computer.id) }

    /// "6 agents · Online" and the route in use, or why the computer is out of reach.
    @ViewBuilder private var subtitle: some View {
        let count = store.roster.count
        HStack(spacing: 4) {
            Text("\(count) agent\(count == 1 ? "" : "s") · \(accounts.statusText(store))")
            if store.connection == .online { RouteIcon(route: store.hostRoute) }
        }
        .font(.subheadline)
        .foregroundStyle(Palette.secondary)
        .lineLimit(2)
    }

    private var routeDetail: String {
        let now = store.hostRoute == .relay ? "Cloudflare" : "Wi-Fi"
        return "\(now) now · or Tailscale, Cloudflare"
    }
}

/// A computer in the account that this iPhone can't use yet.
private struct AccountComputerRow: View {
    let computer: CloudComputer
    let ticket: AccessTicket?
    let ask: () -> Void
    let revoke: () -> Void

    var body: some View {
        HStack(spacing: 12) {
            ComputerTile(Computer(id: computer.computerId, name: computer.name, signKey: computer.signKey, device: computer.device))
                .opacity(0.5)
            VStack(alignment: .leading, spacing: 2) {
                Text(computer.name).font(.body.weight(.semibold)).foregroundStyle(Palette.text)
                Text(detail).font(.subheadline).foregroundStyle(Palette.secondary).lineLimit(2)
            }
            Spacer(minLength: 8)
            if ticket != nil {
                Button("Code", action: ask).buttonStyle(.secondary)
            } else if computer.access == .granted {
                Button("Revoke", role: .destructive, action: revoke).buttonStyle(.secondary)
            } else {
                Button("Ask for access", action: ask).buttonStyle(.secondary)
            }
        }
        .padding(.vertical, 2)
    }

    private var detail: String {
        let online = computer.isOnline ? "Online" : "Offline"
        if let ticket { return "Waiting for your OK on the computer · \(ticket.code)" }
        return switch computer.access {
        case .granted: "Approved for an earlier install of Kith. Revoke it, then ask again."
        case .pending: "\(online) · A request is waiting"
        // none (also what a word this version doesn't know reads as) or no device: not access.
        case .noAccess, nil: "no access yet"
        }
    }
}

/// The same machine under an earlier identity: one tap cleans it out of the account.
private struct OlderCopyRow: View {
    let computer: CloudComputer
    let remove: () -> Void

    var body: some View {
        HStack(spacing: 12) {
            ComputerTile(Computer(id: computer.computerId, name: computer.name, signKey: computer.signKey, device: computer.device))
                .opacity(0.35)
            VStack(alignment: .leading, spacing: 2) {
                Text(computer.name).font(.body.weight(.semibold)).foregroundStyle(Palette.secondary)
                Text(detail).font(.subheadline).foregroundStyle(Palette.tertiary).lineLimit(2)
            }
            Spacer(minLength: 8)
            IconButton("Remove from account", systemImage: "trash", action: remove)
        }
        .padding(.vertical, 2)
    }

    private var detail: String {
        let seen = computer.lastSeenAt.map { " · last seen \(RelativeTime.day(Date(milliseconds: $0)))" } ?? ""
        return "Older copy of this computer\(seen)"
    }
}

