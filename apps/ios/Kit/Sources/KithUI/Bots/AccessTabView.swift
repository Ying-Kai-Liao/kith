import KithKit
import SwiftUI

/// The agent sheet's Access tab (docs/design/DESIGN.md, mocks/access.jpg): Accounts, Keys, Asks before, People.
/// "Allowed in auto mode" lists what Always allow remembered, with Remove.
struct AccessTabView: View {
    let store: AccessStore
    let kith: KithInfoStore?

    var body: some View {
        VStack(alignment: .leading, spacing: 24) {
            accounts
            keys
            asksBefore
            allowed
            people
            if let error = store.removeError {
                Text("Not removed: \(error)").font(.footnote).foregroundStyle(Palette.danger)
            }
        }
    }

    // MARK: accounts and keys

    @ViewBuilder private var accounts: some View {
        switch store.keys {
        case .hidden: EmptyView()
        case .loading: CardSection("Accounts") { SheetNote(text: "Loading…") }
        case .failed(let message): CardSection("Accounts") { SheetNote(text: "Couldn't read the accounts: \(message)", danger: true) }
        case .loaded:
            CardSection("Accounts", footer: store.held.isEmpty ? nil : "Remove takes this agent off the account. The account stays for the others.") {
                if store.held.isEmpty {
                    SheetNote(text: "No accounts yet")
                } else {
                    ForEach(store.held) { key in
                        SheetRow(icon: "person.crop.circle", title: AccessTab.accountTitle(key)) {
                            removeButton(busy: store.removing.contains("key/" + key.profile)) { await store.revoke(key) }
                        }
                    }
                }
            }
        }
    }

    @ViewBuilder private var keys: some View {
        if case .loaded = store.keys, !store.held.isEmpty {
            CardSection("Keys") {
                ForEach(store.held) { key in
                    SheetRow(icon: "key", title: key.profile,
                             detail: AccessTab.keyDetail(key) { kithAgo($0, now: $1) })
                }
            }
        }
    }

    // MARK: asks before

    /// Claude Code's permission modes as a stock menu, written through to hq.
    @ViewBuilder private var asksBefore: some View {
        if let kith, let info = kith.info {
            let current = HqSetting.permissionMode(info.permissionMode ?? "auto")
            CardSection("Asks before", footer: kith.permissionError.map { "Permission not changed: \($0)" }
                        ?? "Kept by Kith on this computer. Every chat of this agent uses it from its next turn.") {
                ValueRow("Mode", detail: HqSetting.permissionDetail(current)) {
                    ChoicePicker(
                        selection: Binding(
                            get: { current },
                            set: { pick in
                                guard pick != current else { return }
                                Task { await kith.setPermission(pick) }
                            }),
                        options: HqSetting.permissionModes.contains { $0.id == current }
                            ? HqSetting.permissionModes : HqSetting.permissionModes + [(id: current, label: HqSetting.permissionLabel(current))],
                        detail: { HqSetting.permissionDetail($0) })
                }
            }
        }
    }

    // MARK: allowed in auto mode

    @ViewBuilder private var allowed: some View {
        switch store.allowed {
        case .hidden: EmptyView()
        case .loading: CardSection("Allowed in auto mode") { SheetNote(text: "Loading…") }
        case .failed(let message): CardSection("Allowed in auto mode") { SheetNote(text: "Couldn't read the allowed list: \(message)", danger: true) }
        case .loaded(let list):
            CardSection("Allowed in auto mode", footer: list.allowed.isEmpty ? nil : "A removed rule is gone at this agent's next run.") {
                if list.allowed.isEmpty && list.error == nil {
                    SheetNote(text: "Nothing yet. “Always allow” on a blocked action lands here.")
                }
                ForEach(list.allowed) { rule in
                    SheetRow(icon: "checkmark.shield", title: rule.text(), detail: rule.detail { kithAgo($0, now: $1) }) {
                        removeButton(busy: store.removing.contains("allowed/" + rule.id)) { await store.remove(rule) }
                    }
                }
                if let error = list.error {
                    SheetNote(text: "Some rules could not be read: \(error)", danger: true)
                }
            }
        }
    }

    // MARK: people

    @ViewBuilder private var people: some View {
        switch store.ledger {
        case .hidden: EmptyView()
        case .loading: CardSection("People") { SheetNote(text: "Loading…") }
        case .failed(let message): CardSection("People") { SheetNote(text: "Couldn't read who may use this agent: \(message)", danger: true) }
        case .loaded(let grants):
            CardSection("People", footer: grants.isEmpty ? nil : "Kept until removed. A removed one is gone from their next run.") {
                if grants.isEmpty {
                    SheetNote(text: "Only you use this agent")
                } else {
                    ForEach(AccessTab.people(grants)) { person in
                        VStack(alignment: .leading, spacing: 2) {
                            Text(person.name).font(.system(size: 13.5, weight: .semibold)).foregroundStyle(Palette.ink)
                            if let line = budgetLine(person.id) {
                                Text(line).font(.system(size: 11.5)).foregroundStyle(Palette.muted)
                            }
                        }
                        .frame(maxWidth: .infinity, alignment: .leading)
                        ForEach(person.grants) { grant in
                            SheetRow(icon: grant.kind == "domain" ? "globe" : grant.kind == "service" ? "key" : "folder", title: grant.text,
                                     detail: grant.since.map { "since " + Date(timeIntervalSince1970: $0 / 1000).formatted(.dateTime.day().month(.abbreviated).year()) }) {
                                removeButton(busy: store.removing.contains("grant/" + grant.key)) { await store.remove(grant) }
                            }
                        }
                    }
                }
            }
        }
    }

    /// "$4.20 of $20" when the host reports it; nothing for someone it has no spend for.
    private func budgetLine(_ person: String) -> String? {
        guard let people = store.people, people.people[person] != nil || (people.budgets[person] ?? nil) != nil else { return nil }
        return people.line(for: person)
    }

    private func removeButton(busy: Bool, _ action: @escaping () async -> Void) -> some View {
        Group {
            if busy {
                ProgressView().controlSize(.small)
            } else {
                IconButton("Remove", systemImage: "trash") { Task { await action() } }
            }
        }
    }
}
