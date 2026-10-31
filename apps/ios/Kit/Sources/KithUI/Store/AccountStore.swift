import KithKit
import Foundation
import Observation
import os

private let log = Logger(subsystem: "com.yingkailiao.kith", category: "Account")

/// One bot across an account context's computers, for lists that mix them.
public struct RosterItem: Identifiable, Sendable {
    public var ref: BotReference
    public var bot: Bot
    public var computer: Computer
    public var id: BotReference { ref }
}

/// Account → computers → bots: one `BotStore` per computer this context can reach
/// (QR-paired or approved through the account),
/// plus the account's computers from the cloud that this device can still ask access to.
/// An account sees every computer it owns or was invited to; each keeps its own role, roster, cache and prompts.
@MainActor
@Observable
public final class AccountStore {
    public let storage: SharedStore.Context
    public var accountId: String? { storage.accountID }
    public private(set) var computers: [Computer] = []
    public private(set) var stores: [ComputerID: BotStore] = [:]
    /// Everything in the account, including computers this device isn't authorized on yet.
    public private(set) var cloudComputers: [CloudComputer] = []
    /// Access requests waiting for approval on the computer, with the SAS to show.
    public private(set) var pendingAccess: [ComputerID: AccessTicket] = [:]
    /// Computers this account joined by an invite, not owns. Learned from the cloud's `role`, and from the invite token
    /// kept here (which also covers the time before the cloud list has loaded or while it is unreachable).
    public private(set) var memberComputers: Set<ComputerID> = []
    public var lastError: String?
    public var selection: BotReference? {
        didSet { if let id = selection?.computerId, !retired { storage.lastComputerId = id } }
    }

    public var onBotUpdated: (@MainActor (BotReference, Bot) -> Void)?
    public var onConnected: (@MainActor (BotStore) -> Void)?
    public var onSent: (@MainActor (BotReference, Bot, SendProgress) -> Void)?
    /// A computer's `call` event (see `BotStore.onCallEvent`).
    public var onCallEvent: (@MainActor (ComputerID, IncomingCallEvent) -> Void)?
    /// The combined roster changed: a bot came, changed or went, or a computer did.
    public var onRosterChanged: (@MainActor ([RosterItem]) -> Void)?

    private let cloud: CloudClient?
    public let inviteTokens: any InviteTokenStore
    private let computerOrder: ComputerOrderStore
    private let makeStore: @MainActor (Computer) -> BotStore
    /// Persisted in the context (channel route).
    private var saved: [Computer]
    private var accessPolls: [ComputerID: Task<Void, Never>] = [:]
    private var isActive = true
    private var retired = false
    /// Asked once per launch: a denied or cancelled request isn't repeated behind the user's back.
    private var asked: Set<ComputerID> = []

    public convenience init(storage: SharedStore.Context, cloud: CloudClient?) {
        self.init(storage: storage, cloud: cloud, inviteTokens: KeychainInviteStore(context: storage)) { computer in
            BotStore(computer: computer, storage: storage)
        }
    }

    init(storage: SharedStore.Context, cloud: CloudClient?, inviteTokens: (any InviteTokenStore)? = nil,
         makeStore: @escaping @MainActor (Computer) -> BotStore) {
        self.storage = storage
        self.cloud = cloud
        self.inviteTokens = inviteTokens ?? KeychainInviteStore(context: storage)
        computerOrder = ComputerOrderStore(storage: storage)
        self.makeStore = makeStore
        saved = storage.computers.filter(\.isConsistent)
        for computer in saved { open(computer) }
        computerOrder.onChanged = { [weak self] in self?.refreshList() }
        refreshList()
    }

    // MARK: derived

    /// Every computer's visible bots, computer by computer, each in its roster order.
    public var roster: [RosterItem] {
        computers.flatMap { computer -> [RosterItem] in
            guard let store = stores[computer.id] else { return [] }
            return store.roster.map {
                RosterItem(ref: BotReference(accountId: accountId, computerId: computer.id, botId: $0.id), bot: $0, computer: store.computer)
            }
        }
    }

    public func store(for id: ComputerID) -> BotStore? { stores[id] }

    /// What this account is on a computer. Owner unless the cloud or a kept invite token says member.
    public func role(for id: ComputerID) -> ComputerRole {
        // A role this version doesn't know already reads as member (the contract's ComputerRole): nothing owner-only shows.
        if let role = cloudComputers.first(where: { $0.computerId == id })?.role { return role }
        return memberComputers.contains(id) ? .member : .owner
    }

    /// What this account is on a computer, for labels: Owner, or Member / Guest by the agents granted there.
    public func access(for id: ComputerID) -> AccessLevel {
        AccessLevel.of(role: role(for: id), granted: stores[id]?.hello?.grantedAgents)
    }

    /// Cards outside this account's chats waiting for its answer on a computer: guests' for the owner, and a card lane's
    /// for whoever the host names. Nothing here crosses to another computer.
    public func guestPrompts(for id: ComputerID) -> [GuestPrompt] {
        stores[id]?.guestPrompts(isOwner: role(for: id) == .owner) ?? []
    }

    /// Bots from outside waiting for this account's answer on a computer it owns.
    public func botRequests(for id: ComputerID) -> [BotGuest] {
        role(for: id) == .owner ? stores[id]?.botGuests.requests ?? [] : []
    }

    /// Whether a link to this chat opens a guest's request instead: an agent's card lane, or a chat the account has no
    /// bot for on a computer it owns. If it only hasn't synced yet, the request view waits and hands over to the chat
    /// when the bot appears; if the request was answered meanwhile, it says so.
    public func opensGuestPrompt(_ ref: BotReference) -> Bool {
        guard let store = stores[ref.computerId] else { return false }
        return GuestPrompt.isCardLane(ref.botId) || (role(for: ref.computerId) == .owner && store.bots[ref.botId] == nil)
    }

    /// The computers this account owns (or pairs by QR), for the screens only an owner has.
    public var ownedComputers: [Computer] { computers.filter { role(for: $0.id) == .owner } }

    // MARK: computers

    /// QR pairing (§4.1); the computer is saved in this context and connected.
    public func pair(_ pairing: Pairing, deviceName: String, platform: String) async throws -> Computer {
        let identity = try DeviceIdentity.load(context: storage)
        var computer = try await HostConnector.pair(pairing, identity: identity, deviceName: deviceName, platform: platform)
        guard !retired else { throw CancellationError() }
        computer.color = saved.first { $0.id == computer.id }?.color
        save(computer)
        open(computer)
        return computer
    }

    public func forget(_ id: ComputerID) {
        inviteTokens.forget(id)
        memberComputers.remove(id)
        saved.removeAll { $0.id == id }
        persist()
        close(id)
        accessPolls.removeValue(forKey: id)?.cancel()
        setPending(id, nil)
        if selection?.computerId == id { selection = nil }
        refreshList()
    }

    /// Saved with the computer on this device; its channel reconnects over the new order right away.
    public func setRoute(_ id: ComputerID, _ route: ConnectionRoute) {
        let value: ConnectionRoute? = route == .automatic ? nil : route
        if let i = saved.firstIndex(where: { $0.id == id }) {
            saved[i].route = value
            persist()
        }
        stores[id]?.updateComputer { $0.route = value }
        stores[id]?.restartStream()
        refreshList()
    }

    public func setColor(_ id: ComputerID, _ color: String) {
        if let i = saved.firstIndex(where: { $0.id == id }) {
            saved[i].color = color
            persist()
        }
        stores[id]?.updateComputer { $0.color = color }
        refreshList()
    }

    /// Moves a computer section on this device only.
    public func move(_ id: ComputerID, to target: ComputerID) {
        computerOrder.move(id, to: target, available: computers.map(\.id))
    }

    // MARK: account (§4.2 B)

    public func refreshCloud() async {
        guard let cloud, !retired else { return }
        do {
            let list = try await cloud.computers()
            guard !retired else { return }
            if list != cloudComputers { Motion.animate { cloudComputers = list } }
            // Signed in, ask every account computer this device can't use yet for access, without
            // a tap. Computers set to auto-approve let it straight in; the others show the code.
            await askForAccess()
        } catch let error as CloudError where !error.isTransient {
            lastError = error.localizedDescription
        } catch {
            // Runs on every launch and foreground: a dropped connection, a busy cloud or a token
            // not ready yet fixes itself on the next refresh, so only answers that stay reach the dialog.
            log.info("cloud refresh failed: \(error.localizedDescription, privacy: .public)")
        }
    }

    /// Starts an access request; the returned `code` must match the one the computer shows.
    /// Approval is then picked up in the background and the computer joins `stores`.
    public func requestAccess(_ id: ComputerID) async throws -> AccessTicket {
        guard let cloud, let target = cloudComputers.first(where: { $0.computerId == id }) else {
            throw CloudError(status: 404, code: "notFound", message: "That computer isn't in your account.")
        }
        let ticket = try await cloud.requestAccess(id, signKey: target.signKey, hostVersion: target.version)
        guard !retired else { throw CancellationError() }
        setPending(id, ticket)
        accessPolls[id]?.cancel()
        accessPolls[id] = Task { [weak self] in await self?.awaitApproval(ticket, target: target, cloud: cloud) }
        return ticket
    }

    /// An offline account computer named like one this device uses or sees online: most likely the same
    /// machine under an earlier identity (its host keys were reset), left behind in the account.
    public func isOlderCopy(_ computer: CloudComputer) -> Bool {
        guard !computer.isOnline else { return false }
        // The current copy may be QR-paired without being listed in the account.
        return computers.contains { $0.id != computer.computerId && $0.name == computer.name }
            || cloudComputers.contains { other in
                other.computerId != computer.computerId && other.name == computer.name
                    && (stores[other.computerId] != nil || other.isOnline)
            }
    }

    /// A computer this device uses that won't come back as it is: it turned this device away (access
    /// revoked, or its identity changed after a reset or reinstall), or it's offline while a computer
    /// with its name is reachable, the same machine under a new identity. Removing it is the fix.
    public func isStale(_ id: ComputerID) -> Bool {
        guard let store = stores[id] else { return false }
        switch store.connection {
        case .unauthorized:
            return true
        case .computerOffline, .offline:
            let name = store.computer.name
            return stores.values.contains { $0.computer.id != id && $0.computer.name == name && $0.connection == .online }
                || cloudComputers.contains { $0.computerId != id && $0.name == name && $0.isOnline }
        default:
            return false
        }
    }

    /// Access requests appear and settle with an animation, like connection changes.
    private func setPending(_ id: ComputerID, _ ticket: AccessTicket?) {
        guard pendingAccess[id] != ticket else { return }
        Motion.animate { pendingAccess[id] = ticket }
    }

    /// Takes a computer out of the account (its grants and pending requests go with it).
    public func removeFromAccount(_ id: ComputerID) async {
        guard let cloud else { return }
        do {
            try await cloud.removeComputer(id)
            setPending(id, nil)
            await refreshCloud()
        } catch {
            lastError = error.localizedDescription
        }
    }

    private func askForAccess() async {
        let waiting = cloudComputers.filter { c in
            c.isOnline && c.access != .granted && !asked.contains(c.computerId)
                && pendingAccess[c.computerId] == nil && stores[c.computerId] == nil
        }
        for computer in waiting {
            asked.insert(computer.computerId)
            do {
                _ = try await requestAccess(computer.computerId)
            } catch let error as CloudError where !error.isTransient {
                lastError = error.localizedDescription
            } catch {
                // Asked without a tap: a connection or cloud hiccup isn't worth a dialog.
                // The computer stays in the list, where it can be asked by hand.
                log.info("access request failed: \(error.localizedDescription, privacy: .public)")
            }
        }
    }

    /// Withdraws a pending access request; the computer drops it from its approval list.
    public func cancelAccess(_ id: ComputerID) async {
        guard let ticket = pendingAccess[id] else { return }
        accessPolls.removeValue(forKey: id)?.cancel()
        setPending(id, nil)
        do {
            try await cloud?.cancelAccess(ticket.requestId)
        } catch {
            lastError = error.localizedDescription
        }
    }

    private func awaitApproval(_ ticket: AccessTicket, target: CloudComputer, cloud: CloudClient) async {
        while !Task.isCancelled, !retired {
            try? await Task.sleep(for: .seconds(2))
            guard !Task.isCancelled, !retired else { return }
            guard let status = try? await cloud.accessStatus(ticket.requestId) else { continue }
            guard !retired, pendingAccess[ticket.computerId]?.requestId == ticket.requestId else { return }
            switch status.status {
            case .pending:
                continue
            case .approved:
                setPending(ticket.computerId, nil)
                // Pin the key the SAS was computed with; the first hello brings its mailbox key and addresses.
                let computer = Computer(id: ticket.computerId, name: target.name, signKey: ticket.signKey,
                                        cloud: cloud.baseURL, device: target.device, labels: ticket.labels ?? .legacy)
                save(computer)
                open(computer)
                Analytics.shared.capture(.computerPaired, ["via": "account", "computer": Analytics.computerTag(computer.id)])
                await refreshCloud()
                return
            // A state this version doesn't know is over as far as it can tell: never read as approved.
            case .denied, .expired, .cancelled, .unknown:
                setPending(ticket.computerId, nil)
                lastError = status.status == .denied ? "\(target.name) declined the request." : "The request expired. Ask again."
                return
            }
        }
    }

    // MARK: joining by invite

    /// The account joins the computer an invite is for. `proof` is h1 (`InviteLink.proof`), never the token.
    public func acceptInvite(inviteId: String, proof: String) async throws -> InviteAccepted {
        guard let cloud else { throw CloudError(status: 0, code: "signedOut", message: "Sign in to join.") }
        return try await cloud.acceptInvite(inviteId, proof: proof)
    }

    /// Keeps the token before anything else is done with the invite: if it can't be stored, the join stops.
    public func keepInvite(_ invite: StoredInvite, for id: ComputerID) throws {
        try inviteTokens.keep(invite, for: id)
        memberComputers.insert(id)
    }

    /// Asks a computer the account just joined for access. Returns false while the computer is offline: it is asked
    /// again by `refreshCloud` once it is back (that asks every online computer this device can't use yet).
    public func askToJoin(_ id: ComputerID) async throws -> Bool {
        guard let cloud else { throw CloudError(status: 0, code: "signedOut", message: "Sign in to join.") }
        let list = try await cloud.computers()
        guard !retired else { throw CancellationError() }
        if list != cloudComputers { Motion.animate { cloudComputers = list } }
        guard let target = list.first(where: { $0.computerId == id }) else {
            throw CloudError(status: 404, code: "notFound", message: "That computer isn't in your account yet. Try the link again in a moment.")
        }
        if stores[id] != nil || pendingAccess[id] != nil { return true }
        guard target.isOnline else { return false }
        asked.insert(id)
        _ = try await requestAccess(id)
        return true
    }

    /// Waits for the computer to take this device, then for the connection. While the request is pending it waits as
    /// long as an owner's device does: until the cloud says it was approved, declined or expired (in Code mode the
    /// owner compares this device's code first, a member's too). The connection then gets `patience`.
    /// nil when it isn't reachable within that; throws when the computer turns this device away.
    public func connection(to id: ComputerID, patience: Duration = .seconds(45)) async throws -> HostClient? {
        var deadline = ContinuousClock.now + patience
        var wasAsking = false
        while ContinuousClock.now < deadline, !retired {
            if pendingAccess[id] != nil {
                // Still asking: the request's own answer ends the wait, as `awaitApproval` does for an owner.
                wasAsking = true
                deadline = ContinuousClock.now + patience
            } else if stores[id] == nil, wasAsking {
                // The request ended without the computer taking this device: declined, expired or cancelled.
                let name = cloudComputers.first { $0.computerId == id }?.name ?? "The computer"
                throw HostError.unauthorized("\(name) didn't let this device in. Ask its owner, then open the link again.")
            }
            if let store = stores[id] {
                switch store.connection {
                case .online:
                    if let client = store.client { return client }
                case let .unauthorized(message):
                    throw HostError.unauthorized(message)
                default:
                    break
                }
            }
            try await Task.sleep(for: .milliseconds(500))
        }
        return nil
    }

    // MARK: lifecycle

    public func setActive(_ active: Bool) {
        guard !retired else { return }
        isActive = active
        for store in stores.values { store.setActive(active) }
    }

    /// Account switched or signed out: nothing from here writes into storage or calls back anymore.
    public func retire() {
        retired = true
        computerOrder.retire()
        for store in stores.values { store.retire() }
        for poll in accessPolls.values { poll.cancel() }
        accessPolls = [:]
        onBotUpdated = nil
        onConnected = nil
        onSent = nil
        onCallEvent = nil
        onRosterChanged = nil
        selection = nil
    }

    // MARK: plumbing

    private func open(_ computer: Computer) {
        close(computer.id)
        let store = makeStore(computer)
        let id = computer.id
        store.onBotUpdated = { [weak self] bot in
            guard let self, !self.retired else { return }
            self.onBotUpdated?(BotReference(accountId: self.accountId, computerId: id, botId: bot.id), bot)
        }
        store.onSent = { [weak self] bot, progress in
            guard let self, !self.retired else { return }
            self.onSent?(BotReference(accountId: self.accountId, computerId: id, botId: bot.id), bot, progress)
        }
        store.onCallEvent = { [weak self] event in
            guard let self, !self.retired else { return }
            self.onCallEvent?(id, event)
        }
        store.onConnected = { [weak self] store in
            guard let self, !self.retired else { return }
            self.onConnected?(store)
        }
        store.onComputerChanged = { [weak self] computer in self?.computerChanged(computer) }
        store.onRosterChanged = { [weak self] in self?.rosterChanged() }
        // Before the store starts: an invited person's first call to the computer shows the token.
        if inviteTokens.stored(for: id) != nil { memberComputers.insert(id) }
        store.inviteTokens = inviteTokens
        stores[id] = store
        if isActive { store.setActive(true) }
    }

    private func close(_ id: ComputerID) {
        stores.removeValue(forKey: id)?.retire()
    }

    private func computerChanged(_ computer: Computer) {
        guard !retired else { return }
        if let i = saved.firstIndex(where: { $0.id == computer.id }) {
            saved[i] = computer
            persist()
        }
        refreshList()
    }

    private func save(_ computer: Computer) {
        saved.removeAll { $0.id == computer.id }
        saved.insert(computer, at: 0)
        persist()
        refreshList()
    }

    private func persist() {
        guard !retired else { return }
        storage.computers = saved
    }

    private func refreshList() {
        let byID = Dictionary(saved.map { ($0.id, $0) }, uniquingKeysWith: { _, newest in newest })
        let ordered = computerOrder.state.orderedIDs(saved.map(\.id)).compactMap { byID[$0] }
        if saved != ordered {
            saved = ordered
            persist()
        }
        if ordered != computers { Motion.animate { computers = ordered } }
        rosterChanged()
    }

    private func rosterChanged() {
        guard !retired else { return }
        onRosterChanged?(roster)
    }
}
