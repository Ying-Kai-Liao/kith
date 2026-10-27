import KithKit
import SwiftUI

/// Settings → Backups: the order Kith moves a bot's turns to when its own account runs out of usage, and which
/// entries are out until when. Read-only on the phone: the order is edited on the computer, where logging in
/// happens too (Kith → Settings → Usage).
public struct BackupsStatusSection: View {
    @Environment(AccountStore.self) private var accounts

    public init() {}

    public var body: some View {
        // Backups are the owner's: an invited person's computer has none to show.
        let online = accounts.ownedComputers.compactMap { accounts.store(for: $0.id) }.filter { $0.connection == .online }
        ForEach(online, id: \.computer.id) { store in
            BackupsComputerSection(store: store, labeled: online.count > 1)
        }
    }
}

/// One computer's backups: Settings → Computers → the computer's page.
public struct BackupsComputerSection: View {
    let store: BotStore
    let labeled: Bool
    @State private var phase: Phase = .loading

    public init(store: BotStore, labeled: Bool) {
        self.store = store
        self.labeled = labeled
    }

    private enum Phase {
        case loading
        case loaded(Backups)
        case failed(String)
        /// A host from before the call: nothing to show.
        case unsupported
    }

    public var body: some View {
        Group {
            switch phase {
            case .unsupported:
                EmptyView()
            case .loading:
                CardSection(title, footer: footer) { Text("Reading the backups…").foregroundStyle(Palette.secondary) }
            case .failed(let message):
                CardSection(title, footer: footer) { Text(message).foregroundStyle(Palette.danger) }
            case .loaded(let backups):
                CardSection(title, footer: footer) {
                    MainAccountRow(backups: backups)
                    if backups.global.isEmpty {
                        Text("No backups. Add one on the computer.").foregroundStyle(Palette.secondary)
                    }
                    ForEach(Array(backups.global.enumerated()), id: \.element) { index, id in
                        BackupRow(index: index + 1, entry: backups.entry(id))
                    }
                }
            }
        }
        .task(id: store.computer.id) { await load() }
    }

    private var title: String { labeled ? "Backups on \(store.hostName)" : "Backups" }
    private var footer: String {
        "Turns start on the main account. When it runs out of usage, they move down this list until the limit lifts. Change both on the computer."
    }

    private func load() async {
        do {
            phase = .loaded(try await store.ready().backups())
        } catch {
            let text = error.localizedDescription
            phase = text.lowercased().contains("unknown method") ? .unsupported : .failed(text)
        }
    }
}

/// The account turns start on, above the chain: the computer's, then each agent that has one of its own.
private struct MainAccountRow: View {
    let backups: Backups

    var body: some View {
        let entry = backups.mainGlobal.map { backups.entry($0) }
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 8) {
                Text("Main account").foregroundStyle(Palette.secondary)
                Text(backups.mainGlobal ?? "default login").lineLimit(1)
                Spacer(minLength: 8)
                if let entry {
                    Text(entry.state()).font(.footnote).foregroundStyle(Palette.secondary).multilineTextAlignment(.trailing)
                }
            }
            ForEach(backups.ownMains, id: \.agent) { own in
                HStack(spacing: 8) {
                    Text(own.agent).font(.footnote).foregroundStyle(Palette.tertiary).lineLimit(1)
                    Text(own.main.id).font(.footnote).lineLimit(1)
                    Spacer(minLength: 8)
                    Text(own.main.state).font(.footnote).foregroundStyle(Palette.secondary)
                }
                if !own.main.ready, let fix = own.main.fix {
                    Text("On the computer: \(fix)")
                        .font(.footnote.monospaced())
                        .foregroundStyle(Palette.secondary)
                        .textSelection(.enabled)
                }
            }
        }
    }
}

private struct BackupRow: View {
    let index: Int
    let entry: BackupEntry

    var body: some View {
        let out = entry.isOut()
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 8) {
                Text("\(index)").foregroundStyle(Palette.tertiary).monospacedDigit()
                Text(entry.id).lineLimit(1)
                Spacer(minLength: 8)
                Text(entry.state())
                    .font(.footnote)
                    .foregroundStyle(out ? Palette.danger : Palette.secondary)
                    .multilineTextAlignment(.trailing)
            }
            if !entry.ready, !out, let fix = entry.fix {
                Text("On the computer: \(fix)")
                    .font(.footnote.monospaced())
                    .foregroundStyle(Palette.secondary)
                    .textSelection(.enabled)
            }
        }
    }
}
