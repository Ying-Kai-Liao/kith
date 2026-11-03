import KithKit
import SwiftUI

/// Settings → Computers → the computer's page → Accounts: the computer's managed account set (which harness accounts
/// turns may use, in what order, the policy, each agent's own list, pins and cross-harness). Every write answers with
/// the fresh read, which replaces what is shown. Adding an account is not here: its login runs in a terminal on the
/// computer. A host from before the `accounts` methods shows the read-only backups instead.
public struct HarnessAccountsSection: View {
    let store: BotStore
    let labeled: Bool
    @State private var phase: Phase = .loading
    @State private var busy = false
    @State private var error: String?
    @State private var removing: HarnessAccount?

    public init(store: BotStore, labeled: Bool) {
        self.store = store
        self.labeled = labeled
    }

    private enum Phase {
        case loading
        case loaded(HarnessAccounts)
        case failed(String)
        /// A host from before the `accounts` methods.
        case unsupported
    }

    public var body: some View {
        Group {
            switch phase {
            case .unsupported:
                BackupsComputerSection(store: store, labeled: labeled)
            case .loading:
                CardSection(title) { Text("Reading the accounts…").foregroundStyle(Palette.secondary) }
            case .failed(let message):
                CardSection(title) {
                    Text(message).foregroundStyle(Palette.danger)
                    Button("Try again") { Task { await load() } }
                }
            case .loaded(let accounts):
                content(accounts)
            }
        }
        .task(id: store.computer.id) { await load() }
        .kithDialog("Remove \(removing?.id ?? "this account")?", isPresented: removingBinding,
                    message: "It leaves every list and its login on the computer is removed. Add it again on the computer.") {
            [DialogAction("Remove", destructive: true) {
                if let a = removing { run { try await $0.accountRemove(id: a.id) } }
            }]
        }
    }

    private var title: String { labeled ? "Accounts on \(store.hostName)" : "Accounts" }

    private var removingBinding: Binding<Bool> {
        Binding(get: { removing != nil }, set: { if !$0 { removing = nil } })
    }

    @ViewBuilder private func content(_ accounts: HarnessAccounts) -> some View {
        let set = accounts.set
        CardSection(title, footer: "Add an account on the computer (Settings → Accounts, or `kith account add`).") {
            HStack(spacing: 8) {
                Text("Policy").foregroundStyle(Palette.secondary)
                Spacer(minLength: 8)
                Picker("Policy", selection: Binding(get: { accounts.policy }, set: { p in
                    run { try await $0.accountPolicy(p) }
                })) {
                    Text("Fill").tag("fill")
                    Text("Spread").tag("spread")
                }
                .pickerStyle(.segmented)
                .frame(maxWidth: 180)
            }
            Text(accounts.policy == "spread"
                 ? "Spread: turns share the accounts in the set."
                 : "Fill: use the accounts in order until each is out.")
                .font(.footnote).foregroundStyle(Palette.secondary)
            Toggle("Cross-harness", isOn: Binding(get: { accounts.crossHarness }, set: { v in
                run { try await $0.accountCrossHarness(value: v ? "on" : "off") }
            }))
            Text("Turns may move to another harness only if Codex is in that agent's set.")
                .font(.footnote).foregroundStyle(Palette.secondary)
            if let error { Text(error).font(.footnote).foregroundStyle(Palette.danger) }
        }
        .disabled(busy)

        CardSection("In the set") {
            if set.isEmpty { Text("No accounts in the set.").foregroundStyle(Palette.secondary) }
            ForEach(Array(set.enumerated()), id: \.element.id) { index, account in
                AccountSetRow(account: account, index: index, count: set.count, busy: busy,
                              move: { delta in
                                  var ids = set.map(\.id)
                                  ids.swapAt(index, index + delta)
                                  run { try await $0.accountsSet(ids: ids) }
                              },
                              toggleSet: { run { try await $0.accountsSet(ids: set.map(\.id).filter { $0 != account.id }) } },
                              back: { run { try await $0.accountClear(id: account.id) } },
                              remove: { removing = account })
            }
        }
        if !accounts.outsideSet.isEmpty {
            CardSection("Not in the set") {
                ForEach(accounts.outsideSet) { account in
                    AccountSetRow(account: account, index: nil, count: set.count, busy: busy, move: { _ in },
                                  toggleSet: { run { try await $0.accountsSet(ids: set.map(\.id) + [account.id]) } },
                                  back: { run { try await $0.accountClear(id: account.id) } },
                                  remove: { removing = account })
                }
            }
        }
        if !accounts.agentNames.isEmpty {
            CardSection("Agents") {
                ForEach(accounts.agentNames, id: \.self) { name in
                    if let agent = accounts.agents[name] {
                        AgentAccountsRow(name: name, agent: agent, accounts: accounts, busy: busy, run: run)
                    }
                }
            }
        }
    }

    private func load() async {
        do {
            phase = .loaded(try await store.ready().accounts())
        } catch {
            let text = error.localizedDescription
            phase = isUnknownMethod(error) ? .unsupported : .failed(text)
        }
    }

    /// One write at a time: buttons are disabled while it runs, and its answer is the new state.
    private func run(_ write: @escaping (HostClient) async throws -> HarnessAccounts) {
        guard !busy else { return }
        busy = true
        error = nil
        Task {
            do {
                phase = .loaded(try await write(try await store.ready()))
            } catch {
                self.error = error.localizedDescription
            }
            busy = false
        }
    }
}

/// One account: its harness, name, state, usage windows, and what can be done to it.
struct AccountSetRow: View {
    let account: HarnessAccount
    /// Position in the set, nil when it isn't in it.
    let index: Int?
    let count: Int
    let busy: Bool
    let move: (Int) -> Void
    let toggleSet: () -> Void
    let back: () -> Void
    let remove: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                if let index { Text("\(index + 1)").foregroundStyle(Palette.tertiary).monospacedDigit() }
                VStack(alignment: .leading, spacing: 0) {
                    Text(account.account.isEmpty ? account.id : account.account).lineLimit(1)
                    Text(account.harness).font(.footnote).foregroundStyle(Palette.tertiary)
                }
                Spacer(minLength: 8)
                Text(account.state())
                    .font(.footnote)
                    .foregroundStyle(account.status == .ready ? Palette.secondary : Palette.danger)
                    .multilineTextAlignment(.trailing)
            }
            if account.status == .signedOut, let fix = account.fix {
                Text("On the computer: \(fix)").font(.footnote.monospaced()).foregroundStyle(Palette.secondary)
                    .textSelection(.enabled)
            }
            ForEach(account.windows, id: \.name) { w in
                HStack(spacing: 6) {
                    Text(w.name)
                    if let p = w.percent { Text("\(Int(p.rounded()))%").monospacedDigit() }
                    if let r = w.resetsAt { Text("resets \(r.formatted(date: .omitted, time: .shortened))") }
                }
                .font(.footnote).foregroundStyle(Palette.secondary)
            }
            if !account.pins.isEmpty {
                Text("Pinned: " + account.pins.map { $0.only ? "\($0.agent) (only)" : $0.agent }.joined(separator: ", "))
                    .font(.footnote).foregroundStyle(Palette.secondary)
            }
            HStack(spacing: 14) {
                if let index {
                    Button { move(-1) } label: { Image(systemName: "chevron.up") }
                        .disabled(busy || index == 0).accessibilityLabel("Move up")
                    Button { move(1) } label: { Image(systemName: "chevron.down") }
                        .disabled(busy || index == count - 1).accessibilityLabel("Move down")
                }
                Button(index == nil ? "Add to set" : "Take out of set", action: toggleSet).disabled(busy)
                if account.status == .out { Button("It's back", action: back).disabled(busy) }
                Spacer(minLength: 0)
                Button("Remove", role: .destructive, action: remove).disabled(busy)
            }
            .font(.footnote)
            .buttonStyle(.borderless)
        }
        .disabled(busy)
    }
}

/// One agent: its order, what a run would try now, its pin, own list or the computer's, and cross-harness.
private struct AgentAccountsRow: View {
    let name: String
    let agent: HarnessAgentAccounts
    let accounts: HarnessAccounts
    let busy: Bool
    let run: (@escaping (HostClient) async throws -> HarnessAccounts) -> Void
    @State private var open = false

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Button { withAnimation { open.toggle() } } label: {
                HStack {
                    Text(name)
                    Text(agent.harness).font(.footnote).foregroundStyle(Palette.tertiary)
                    Spacer(minLength: 8)
                    if agent.allOut { Text("all out").font(.footnote).foregroundStyle(Palette.danger) }
                    Image(systemName: open ? "chevron.up" : "chevron.down").font(.footnote).foregroundStyle(Palette.tertiary)
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            if open { details }
        }
    }

    @ViewBuilder private var details: some View {
        let ownIds = agent.own ?? []
        info("Order", agent.order.isEmpty ? "none" : agent.order.joined(separator: " → "))
        info("Tries now", agent.candidates.isEmpty ? "nothing" : agent.candidates.joined(separator: ", "))
        ForEach(agent.skipped, id: \.id) { s in
            info("Skipped", "\(s.id): " + (s.outUntil.map { "out until \($0.formatted(date: .omitted, time: .shortened))" } ?? s.why ?? s.status ?? ""))
        }
        info("List", agent.own == nil ? "follows the computer's" : "its own")
        HStack(spacing: 14) {
            if agent.own != nil {
                Button("Follow the computer") { run { try await $0.accountsReset(agent: name) } }
            } else {
                Button("Give it its own list") {
                    run { try await $0.accountsSet(agent: name, ids: accounts.set.map(\.id)) }
                }
            }
        }
        .font(.footnote).buttonStyle(.borderless)
        if agent.own != nil {
            ForEach(Array(ownIds.enumerated()), id: \.element) { i, id in
                HStack(spacing: 14) {
                    Text("\(i + 1) \(id)").lineLimit(1)
                    Spacer(minLength: 8)
                    Button { swap(i, i - 1) } label: { Image(systemName: "chevron.up") }.disabled(i == 0)
                    Button { swap(i, i + 1) } label: { Image(systemName: "chevron.down") }.disabled(i == ownIds.count - 1)
                    Button { run { try await $0.accountsSet(agent: name, ids: ownIds.filter { $0 != id }) } } label: {
                        Image(systemName: "minus.circle")
                    }
                }
                .font(.footnote).buttonStyle(.borderless)
            }
            let addable = accounts.accounts.filter { !ownIds.contains($0.id) }
            if !addable.isEmpty {
                Menu("Add to its list") {
                    ForEach(addable) { a in
                        Button(a.id) { run { try await $0.accountsSet(agent: name, ids: ownIds + [a.id]) } }
                    }
                }
                .font(.footnote)
            }
        }
        HStack(spacing: 14) {
            if let pin = agent.pin {
                Text("Pinned to \(pin.id)\(pin.only ? " (only)" : "")").font(.footnote)
                Button("Unpin") { run { try await $0.accountUnpin(agent: name) } }
                Button(pin.only ? "Allow fallback" : "Only this") {
                    run { try await $0.accountPin(agent: name, id: pin.id, only: !pin.only) }
                }
            } else {
                Menu("Pin an account") {
                    ForEach(accounts.accounts.filter { $0.harness == agent.harness }) { a in
                        Button(a.id) { run { try await $0.accountPin(agent: name, id: a.id, only: false) } }
                    }
                }
            }
        }
        .font(.footnote).buttonStyle(.borderless)
        HStack(spacing: 14) {
            Text("Cross-harness: \(agent.crossHarness ? "on" : "off")").font(.footnote)
            Menu("Change") {
                Button("On") { run { try await $0.accountCrossHarness(agent: name, value: "on") } }
                Button("Off") { run { try await $0.accountCrossHarness(agent: name, value: "off") } }
                Button("Follow the computer") { run { try await $0.accountCrossHarness(agent: name, value: "reset") } }
            }
            .font(.footnote)
        }
    }

    private func swap(_ a: Int, _ b: Int) {
        var ids = agent.own ?? []
        guard ids.indices.contains(a), ids.indices.contains(b) else { return }
        ids.swapAt(a, b)
        run { try await $0.accountsSet(agent: name, ids: ids) }
    }

    private func info(_ label: String, _ value: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Text(label).font(.footnote).foregroundStyle(Palette.tertiary)
            Text(value).font(.footnote).foregroundStyle(Palette.secondary)
        }
    }
}
