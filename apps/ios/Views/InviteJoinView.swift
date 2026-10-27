import KithKit
import KithUI
import Observation
import SwiftUI

/// The invitee opening `kith://invite?...`: runs the join and keeps the state the sheet shows.
@MainActor
@Observable
final class InviteJoinModel: Identifiable {
    let link: InviteLink
    private(set) var state: InviteJoinState
    @ObservationIgnored private var task: Task<Void, Never>?
    private unowned let app: AppStore

    nonisolated var id: String { link.inviteId }

    init(link: InviteLink, app: AppStore) {
        self.link = link
        self.app = app
        state = InviteJoinState(.joining, computerId: link.computerId)
    }

    /// Runs the join, or runs it again after it stopped; one run at a time.
    func start() {
        guard task == nil else { return }
        task = Task { [weak self] in
            guard let self else { return }
            await InviteJoin.run(link, deps: AppInviteDeps(app: app)) { self.state = $0 }
            task = nil
        }
    }

    func cancel() {
        task?.cancel()
        task = nil
    }
}

/// The join's dependencies, read from the app when asked: signing in swaps the account's stores for new ones.
@MainActor
private struct AppInviteDeps: InviteJoinDeps {
    let app: AppStore

    /// Signed in, and the app has moved to that account's stores.
    var isSignedIn: Bool { app.account.isSignedIn && app.accounts.accountId == app.account.userID && app.cloud != nil }

    func waitForSignIn() async throws {
        while !isSignedIn {
            try Task.checkCancellation()
            try await Task.sleep(for: .milliseconds(300))
        }
    }

    func accept(inviteId: String, proof: String) async throws -> InviteAccepted {
        try await app.accounts.acceptInvite(inviteId: inviteId, proof: proof)
    }

    func keep(_ invite: StoredInvite, for computer: ComputerID) throws {
        try app.accounts.keepInvite(invite, for: computer)
    }

    func requestAccess(_ computer: ComputerID) async throws -> Bool {
        try await app.accounts.askToJoin(computer)
    }

    func connect(_ computer: ComputerID) async throws -> (any InviteHost)? {
        try await app.accounts.connection(to: computer)
    }
}

/// Joining a computer by invite: each step as it happens, then what the person is on that computer.
struct InviteJoinView: View {
    let model: InviteJoinModel
    @Environment(AccountStore.self) private var accounts
    @Environment(\.dismissModal) private var dismiss

    private var state: InviteJoinState { model.state }
    private var name: String {
        state.computerId.flatMap { id in
            accounts.store(for: id)?.hostName ?? accounts.cloudComputers.first { $0.computerId == id }?.name
        } ?? "the computer"
    }

    var body: some View {
        VStack(spacing: 24) {
            Image(systemName: icon)
                .font(.system(size: 44))
                .foregroundStyle(state.step == .failed ? Palette.danger : state.step == .done ? Palette.added : Palette.secondary)
                .padding(.top, 32)
            Text(title).font(.title3.weight(.semibold)).foregroundStyle(Palette.text).multilineTextAlignment(.center)

            switch state.step {
            case .signingIn:
                Text("Sign in with the account you want to use with this computer.")
                    .font(.subheadline).foregroundStyle(Palette.secondary).multilineTextAlignment(.center)
                SignInButtons()
            case .joining, .asking, .connecting, .proving:
                HStack(spacing: 8) {
                    Spinner()
                    Text(progress).foregroundStyle(Palette.secondary)
                }
                .font(.subheadline)
                if let code {
                    Text(InviteJoin.codeLine(code))
                        .font(.subheadline.monospacedDigit()).foregroundStyle(Palette.text).multilineTextAlignment(.center)
                }
            case .waiting:
                Text(state.message ?? "").font(.subheadline).foregroundStyle(Palette.secondary).multilineTextAlignment(.center)
                Text("You can close this. Kith finishes joining by itself when the computer is reachable.")
                    .font(.footnote).foregroundStyle(Palette.tertiary).multilineTextAlignment(.center)
            case .failed:
                Text(state.message ?? "Couldn't join.").font(.subheadline).foregroundStyle(Palette.danger).multilineTextAlignment(.center)
                Button("Try again") { model.start() }.buttonStyle(.secondary)
            case .done:
                if let id = state.computerId {
                    Text("You can use the agents they picked for you. Open the Bots tab to start a chat.")
                        .font(.subheadline).foregroundStyle(Palette.secondary).multilineTextAlignment(.center)
                    Text(roleLine(id)).font(.footnote).foregroundStyle(Palette.tertiary)
                }
                Button("Done") { dismiss() }.buttonStyle(.primary)
            }
            if state.step != .done {
                Text("This works on this iPhone only. To use another device, open the link there too.")
                    .font(.footnote).foregroundStyle(Palette.tertiary).multilineTextAlignment(.center)
            } else {
                Text("To use another device, open the link there too.")
                    .font(.footnote).foregroundStyle(Palette.tertiary).multilineTextAlignment(.center)
            }
            Spacer()
        }
        .padding(24)
        .frame(maxWidth: .infinity)
        .background(Palette.background)
        .safeAreaInset(edge: .top, spacing: 0) { ModalHeader("Join a computer") }
        .task { model.start() }
    }

    /// This device's six digits while its request is pending (the step is `asking`, then `connecting` while it waits).
    private var code: String? {
        guard state.step == .asking || state.step == .connecting else { return nil }
        return state.computerId.flatMap { accounts.pendingAccess[$0]?.code }
    }

    private func roleLine(_ id: ComputerID) -> String {
        accounts.role(for: id) == .member ? "\(name), member" : name
    }

    private var icon: String {
        switch state.step {
        case .failed: "exclamationmark.triangle"
        case .done: "checkmark.circle.fill"
        case .waiting: "clock"
        default: "person.badge.plus"
        }
    }

    private var title: String {
        switch state.step {
        case .signingIn: "Sign in to join"
        case .failed: "Couldn't join"
        case .done: "You joined \(name)"
        case .waiting: "Almost there"
        default: "Joining \(name)"
        }
    }

    private var progress: String {
        switch state.step {
        case .joining: "Using your invite…"
        case .asking: "Asking \(name) for access…"
        case .connecting where code != nil: "Asking \(name) for access…"
        case .connecting: "Connecting to \(name)…"
        default: "Checking your invite with \(name)…"
        }
    }
}

/// "New chat" on a computer joined by invite: the person picks an agent they were given, and the computer makes their chat.
struct NewChatSheet: View {
    let store: BotStore
    @Environment(AccountStore.self) private var accounts
    @Environment(\.dismissModal) private var dismiss
    @State private var typed = ""
    @State private var busy = false
    @State private var error: String?

    /// The agents of chats this person already has. The computer has no call that lists what was granted, so a new
    /// agent is typed by name.
    private var known: [String] {
        Array(Set(store.bots.values.compactMap(\.hqAgent).filter { !$0.isEmpty })).sorted()
    }

    /// What the computer says was granted; nil from a host that doesn't say (then the fallback below).
    private var granted: [GrantedAgent]? { store.hello?.grantedAgents }

    var body: some View {
        CardForm {
            if let granted {
                CardSection("Your agents", footer: granted.isEmpty ? "\(store.hostName) hasn't given you an agent yet." : nil) {
                    ForEach(granted) { item in
                        Button { Task { await start(item.agent) } } label: {
                            HStack {
                                Label(item.title, systemImage: "bubble.left")
                                Spacer()
                                Text(item.role.title).font(.subheadline).foregroundStyle(Palette.secondary)
                            }
                            .foregroundStyle(Palette.text)
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .disabled(busy)
                    }
                }
                if let error { Text(error).font(.footnote).foregroundStyle(Palette.danger) }
            } else {
                fallback
            }
        }
        .background(Palette.background)
        .safeAreaInset(edge: .top, spacing: 0) { ModalHeader("New chat") }
    }

    @ViewBuilder private var fallback: some View {
        Group {
            if !known.isEmpty {
                CardSection("Your agents") {
                    ForEach(known, id: \.self) { agent in
                        Button { Task { await start(agent) } } label: {
                            Label(agent, systemImage: "bubble.left").foregroundStyle(Palette.text)
                                .frame(maxWidth: .infinity, alignment: .leading).contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .disabled(busy)
                    }
                }
            }
            CardSection("Another agent", footer: "Type the name of an agent \(store.hostName) gave you.") {
                TextField("Agent name", text: $typed)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .submitLabel(.go)
                    .onSubmit { Task { await start(typed) } }
            }
            if let error {
                Text(error).font(.footnote).foregroundStyle(Palette.danger)
            }
            Button {
                Task { await start(typed) }
            } label: {
                ZStack {
                    Text("Start chat").opacity(busy ? 0 : 1)
                    if busy { Spinner(size: 18) }
                }
                .frame(maxWidth: .infinity, minHeight: 28)
            }
            .buttonStyle(.primary)
            .disabled(busy || typed.trimmingCharacters(in: .whitespaces).isEmpty)
        }
    }

    private func start(_ agent: String) async {
        let agent = agent.trimmingCharacters(in: .whitespaces)
        guard !busy, !agent.isEmpty else { return }
        busy = true
        error = nil
        defer { busy = false }
        do {
            let bot = try await store.startChat(agent: agent)
            accounts.selection = BotReference(accountId: accounts.accountId, computerId: store.computer.id, botId: bot.id)
            dismiss()
        } catch {
            // The computer's own words, such as "Chatting with agents isn't open to guests and members yet."
            self.error = error.localizedDescription
        }
    }
}
