import KithKit
import KithUI
import SwiftUI

extension BotStore {
    /// One short line for a computer's connection, used next to its name.
    var statusText: String {
        switch connection {
        case .online where mismatch != nil: "Needs update"
        case .online: "Online"
        case .connecting: "Connecting…"
        case let .computerOffline(lastSeen): lastSeen.map { "Offline · seen \(RelativeTime.day($0))" } ?? "Offline"
        case .offline: "Can't reach"
        case let .unauthorized(message): message
        case .unpaired: "Not paired"
        }
    }
}

extension AccountStore {
    /// A computer's status line; one that's offline while its new identity is around says so.
    func statusText(_ store: BotStore) -> String {
        if case .unauthorized = store.connection { return store.statusText }
        return isStale(store.computer.id) ? "Older copy of this computer · \(store.statusText)" : store.statusText
    }
}

/// How the phone reaches a computer right now: a subtle icon, direct Wi-Fi/LAN or the encrypted relay.
struct RouteIcon: View {
    let route: HostRoute?

    var body: some View {
        switch route {
        case .direct:
            Image(systemName: "wifi").accessibilityLabel("Wi-Fi or Tailscale")
        case .relay:
            Image(systemName: "cloud").accessibilityLabel("Through Cloudflare")
        case nil:
            EmptyView()
        }
    }
}

/// A computer in its own colour: a rounded square with a white display, as in the Computers list.
struct ComputerTile: View {
    let computer: Computer?
    let size: CGFloat

    init(_ computer: Computer?, size: CGFloat = 30) {
        self.computer = computer
        self.size = size
    }

    var body: some View {
        RoundedRectangle(cornerRadius: size * 0.27, style: .continuous)
            .fill(computer?.color.map(AvatarPalette.color) ?? Palette.accent)
            .frame(width: size, height: size)
            .overlay {
                Image(systemName: "display")
                    .font(.system(size: size * 0.5, weight: .medium))
                    .foregroundStyle(.white)
            }
            .accessibilityHidden(true)
    }
}

/// One computer's page: everything its long-press menu holds, as rows, plus order, Reconnect and Remove.
struct ComputerPage: View {
    let store: BotStore
    /// Set when this iPhone's access to the computer can be revoked.
    let revokeTarget: CloudComputer?
    let openScreen: () -> Void
    let openMarketplace: () -> Void
    @Environment(AppStore.self) private var app
    @Environment(AccountStore.self) private var accounts
    @Environment(\.dismiss) private var pop
    @AppStorage("hiddenComputers") private var hiddenComputers = ""
    @State private var voice = false
    @State private var confirmRevoke = false
    @State private var confirmRemove = false

    private var id: ComputerID { store.computer.id }
    private var online: Bool { store.connection == .online }
    private var owned: Bool { accounts.role(for: id) != .member }
    private var stale: Bool { accounts.isStale(id) }
    private var index: Int? { accounts.computers.firstIndex { $0.id == id } }

    var body: some View {
        let selection = ComputerSelection(all: accounts.computers.map(\.id), hidden: hiddenComputers)
        CardForm {
            CardSection {
                HStack(spacing: 12) {
                    ComputerTile(store.computer, size: 40)
                    VStack(alignment: .leading, spacing: 2) {
                        Text("\(store.hostName) · \(accounts.access(for: id).title)")
                            .font(.body.weight(.semibold)).foregroundStyle(Palette.text).lineLimit(1)
                        HStack(spacing: 4) {
                            Text(online ? "Online" : accounts.statusText(store))
                            if online { RouteIcon(route: store.hostRoute) }
                        }
                        .font(.subheadline)
                        .foregroundStyle(store.isOffline ? Palette.warning : Palette.secondary)
                    }
                }
            }

            CardSection("Show in Crew", footer: "Rooms from every shown computer appear together in Crew. This only changes this iPhone.") {
                Toggle(isOn: Binding(get: { selection.shown.contains(id) },
                                     set: { _ in withAnimation(Motion.layout) { hiddenComputers = selection.toggling(id) } })) {
                    Text("Show in Crew")
                }
                .toggleStyle(.kithGreen)
                if let index, accounts.computers.count > 1 {
                    ValueRow("Order", detail: "Position in the list of computers on this iPhone.") {
                        HStack(spacing: 4) {
                            IconButton("Move up", systemImage: "chevron.up") {
                                withAnimation(Motion.layout) { accounts.move(id, to: accounts.computers[index - 1].id) }
                            }
                            .disabled(index == 0)
                            IconButton("Move down", systemImage: "chevron.down") {
                                withAnimation(Motion.layout) { accounts.move(id, to: accounts.computers[index + 1].id) }
                            }
                            .disabled(index == accounts.computers.count - 1)
                        }
                    }
                }
            }

            CardSection("Colour") {
                SwatchPanel(selected: store.computer.color) { accounts.setColor(id, $0) }
            }

            if !stale {
                CardSection("Route", footer: "Which way this iPhone reaches the computer, and whether Cloudflare is a fallback at all.") {
                    MenuRow("Route", systemImage: "wifi",
                            selection: Binding(get: { store.computer.route ?? .automatic }, set: { accounts.setRoute(id, $0) }),
                            options: ConnectionRoute.allCases.map { ($0, $0.title) })
                    Button { store.restartStream() } label: {
                        Label("Reconnect", systemImage: "arrow.clockwise")
                            .foregroundStyle(Palette.text)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                }
            }

            if online {
                CardSection {
                    Button(action: openMarketplace) { LinkRow { Label("Extensions", systemImage: "square.grid.2x2") } }
                        .buttonStyle(.plain)
                    Button { voice = true } label: { LinkRow { Label("Voice chat", systemImage: "waveform") } }
                        .buttonStyle(.plain)
                    if store.screen != nil {
                        Button(action: openScreen) { LinkRow { Label("Show screen", systemImage: "display") } }
                            .buttonStyle(.plain)
                    }
                }
                if owned { HarnessAccountsSection(store: store, labeled: false) }
            }

            CardSection {
                if revokeTarget != nil {
                    Button { confirmRevoke = true } label: {
                        Label("Revoke this iPhone's access", systemImage: "lock.slash")
                            .foregroundStyle(Palette.danger)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                }
                Button { confirmRemove = true } label: {
                    Label(stale ? "Remove this older copy" : "Remove", systemImage: "trash")
                        .foregroundStyle(Palette.danger)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            }
        }
        .navigationDestination(isPresented: $voice) { VoiceChatSettingsView(pushed: true).environment(store) }
        .kithDialog("Revoke this iPhone's access to \(revokeTarget?.name ?? "the computer")?", isPresented: $confirmRevoke,
                    message: "You can ask for access again; the computer will show a new code to confirm.") {
            [DialogAction("Revoke", destructive: true) {
                if let c = revokeTarget { Task { await app.revokeAccess(c.computerId) } }
            }]
        }
        .kithDialog("Remove \(store.hostName)?", isPresented: $confirmRemove,
                    message: "Its bots and conversations stay on that computer. You can pair again any time.") {
            [DialogAction("Remove", destructive: true) {
                accounts.forget(id)
                pop()
            }]
        }
        .page(store.hostName, pushed: true)
    }
}
