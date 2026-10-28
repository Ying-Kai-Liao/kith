import Foundation
import Testing
@testable import KithKit
@testable import KithUI

// Test tokens are obviously fake and low-entropy on purpose: realistic-looking secrets get flagged by scanners.
private let fakeToken = String(repeating: "a", count: 43)

private final class Tokens: InviteTokenStore, @unchecked Sendable {
    var invites: [ComputerID: StoredInvite] = [:]
    func stored(for computer: ComputerID) -> StoredInvite? { invites[computer] }
    func keep(_ invite: StoredInvite, for computer: ComputerID) throws { invites[computer] = invite }
    func forget(_ computer: ComputerID) { invites[computer] = nil }
}

private func mac(_ name: String, keyByte: UInt8) -> Computer {
    let key = Data(repeating: keyByte, count: 32)
    let encoded = key.base64EncodedString().replacingOccurrences(of: "+", with: "-")
        .replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
    return Computer(id: RelayCrypto.computerId(signKey: key), name: name, signKey: encoded)
}

private func bot(_ id: String, agent: String? = nil, kind: String = "agent") throws -> Bot {
    let agentField = agent.map { #","hqAgent":"\#($0)""# } ?? ""
    return try JSONDecoder().decode(Bot.self, from: Data(#"{"id":"\#(id)","name":"Bot \#(id)","kind":"\#(kind)","rev":1\#(agentField)}"#.utf8))
}

private func hello(granted: String?) throws -> Hello {
    let base = #""hostId":"h1","name":"Mac","version":"0.1.0","protocol":1,"minProtocol":1,"os":"macos","rev":4"#
    let extra = granted.map { #","grantedAgents":\#($0)"# } ?? ""
    return try JSONDecoder().decode(Hello.self, from: Data("{\(base)\(extra)}".utf8))
}

private func card(_ id: String, chat: String, status: String = "pending", approver: String? = "owner", at: Int64 = 1) -> Entry {
    var data = EntryData(text: nil, status: status)
    data.title = "Run a command"
    data.approver = approver
    data.person = "sam"
    data.personName = "Sam"
    data.agent = "house"
    data.options = [
        PermissionOption(optionId: "reject", name: "No", kind: "reject_once"),
        PermissionOption(optionId: "allow", name: "Yes", kind: "allow_once"),
    ]
    return Entry(id: id, seq: at, botId: chat, rev: at, kind: "permission", turn: 1, data: data, createdAt: at, updatedAt: at)
}

// MARK: - the role on each computer

@Test func accessLevelIsOwnerThenMemberIfAnyAgentIsAMembersElseGuest() {
    let guest = GrantedAgent(agent: "house", role: .guest)
    let member = GrantedAgent(agent: "work", role: .member)
    #expect(AccessLevel.of(role: .owner, granted: nil) == .owner)
    #expect(AccessLevel.of(role: .member, granted: [guest, member]) == .member)
    #expect(AccessLevel.of(role: .member, granted: [guest]) == .guest)
    // Not loaded yet, or nothing granted: the least.
    #expect(AccessLevel.of(role: .member, granted: nil) == .guest)
    #expect(AccessLevel.of(role: .member, granted: []) == .guest)
    #expect(AccessLevel.agentRole("work", granted: [guest, member]) == .member)
    #expect(AccessLevel.agentRole("other", granted: [guest, member]) == nil)
    #expect(["Owner", "Member", "Guest"] == [AccessLevel.owner, .member, .guest].map(\.title))
}

@MainActor @Test func eachComputerKeepsItsOwnRoleAndOwnerScreensStayWithTheOwnersComputer() throws {
    let suite = "MultiMacTests.\(UUID().uuidString)"
    defer { UserDefaults(suiteName: suite)?.removePersistentDomain(forName: suite) }
    let storage = SharedStore.Context(accountID: "user_one", suite: suite)
    let a = mac("A", keyByte: 1), b = mac("B", keyByte: 2), c = mac("C", keyByte: 3)
    storage.computers = [a, b, c]
    let tokens = Tokens()
    tokens.invites[b.id] = StoredInvite(inviteId: "inv_1", token: fakeToken)
    tokens.invites[c.id] = StoredInvite(inviteId: "inv_2", token: fakeToken)
    let accounts = AccountStore(storage: storage, cloud: nil, inviteTokens: tokens) { computer in
        BotStore(computer: computer, storage: storage) { FakeRemote(.connecting) }
    }
    defer { accounts.retire() }
    accounts.setActive(false)
    accounts.store(for: b.id)?.hello = try hello(granted: #"[{"agent":"house","role":"guest"},{"agent":"work","role":"member"}]"#)
    accounts.store(for: c.id)?.hello = try hello(granted: #"[{"agent":"house","role":"guest"}]"#)

    #expect(accounts.access(for: a.id) == .owner)
    #expect(accounts.access(for: b.id) == .member)
    #expect(accounts.access(for: c.id) == .guest)
    // Being an invitee on B and C takes nothing from A.
    #expect(accounts.ownedComputers.map(\.id) == [a.id])
}

// MARK: - the roster only holds what was granted

@MainActor @Test func anInviteesRosterHoldsOnlyTheGrantedAgentsEvenIfTheHostSentMore() throws {
    let suite = "MultiMacTests.\(UUID().uuidString)"
    defer { UserDefaults(suiteName: suite)?.removePersistentDomain(forName: suite) }
    let storage = SharedStore.Context(accountID: "user_two", suite: suite)
    let store = BotStore(computer: mac("B", keyByte: 2), storage: storage) { FakeRemote(.connecting) }
    defer { store.retire() }
    for b in [try bot("one", agent: "house"), try bot("two", agent: "work"), try bot("three"), try bot("grp", kind: "group")] {
        store.bots[b.id] = b
    }
    // An owner's hello has no list: everything shows.
    store.hello = try hello(granted: nil)
    #expect(Set(store.roster.map(\.id)) == ["one", "two", "three", "grp"])

    store.hello = try hello(granted: #"[{"agent":"house","role":"guest"}]"#)
    #expect(store.roster.map(\.id) == ["one"])
    // Taking the agent away leaves the list at once.
    store.hello = try hello(granted: "[]")
    #expect(store.roster.isEmpty)
}

@MainActor @Test func aMemberComputersRosterNeverShowsUnderAnother() throws {
    let suite = "MultiMacTests.\(UUID().uuidString)"
    defer { UserDefaults(suiteName: suite)?.removePersistentDomain(forName: suite) }
    let storage = SharedStore.Context(accountID: "user_three", suite: suite)
    let a = mac("A", keyByte: 1), b = mac("B", keyByte: 2)
    storage.computers = [a, b]
    let tokens = Tokens()
    tokens.invites[b.id] = StoredInvite(inviteId: "inv_1", token: fakeToken)
    let accounts = AccountStore(storage: storage, cloud: nil, inviteTokens: tokens) { computer in
        BotStore(computer: computer, storage: storage) { FakeRemote(.connecting) }
    }
    defer { accounts.retire() }
    accounts.setActive(false)
    // The same bot id on both computers is two bots.
    for id in [a.id, b.id] { accounts.store(for: id)?.bots["same"] = try bot("same", agent: "house") }
    accounts.store(for: b.id)?.bots["extra"] = try bot("extra", agent: "work")
    accounts.store(for: b.id)?.hello = try hello(granted: #"[{"agent":"house","role":"guest"}]"#)

    let byComputer = Dictionary(grouping: accounts.roster, by: \.ref.computerId).mapValues { $0.map(\.bot.id).sorted() }
    #expect(byComputer[a.id] == ["same"])
    #expect(byComputer[b.id] == ["same"])
}

// MARK: - guests' prompts for the owner

@Test func guestPromptsAreThePendingOwnerCardsInChatsTheOwnerHasNoBotFor() {
    let entries = [
        "chat-sam": [card("p1", chat: "chat-sam", at: 5), card("p2", chat: "chat-sam", status: "answered", at: 6),
                     card("p3", chat: "chat-sam", approver: "member", at: 7)],
        "chat-ann": [card("p4", chat: "chat-ann", at: 2)],
        "mine": [card("p5", chat: "mine", approver: nil, at: 1)],
    ]
    let pending = GuestPrompt.pending(in: entries, known: ["mine"])
    #expect(pending.map(\.id) == ["p4", "p1"])
    #expect(pending[1].who == "Sam · house")
    #expect(pending[1].request == "Run a command")
    #expect(pending[1].chatId == "chat-sam")
    // Only what the card offered, allow first.
    #expect(pending[1].options.map(\.optionId) == ["allow", "reject"])
    #expect(pending[1].options.map(GuestPrompt.label) == ["Allow once", "Deny"])
    // The same card inside one of the owner's own bots is that bot's, not a guest's.
    #expect(GuestPrompt.pending(in: entries, known: ["chat-sam", "chat-ann", "mine"]).isEmpty)
}

@Test func theInviteesChatShowsWaitingTextInPlaceOfButtons() {
    let waiting = card("p1", chat: "c")
    #expect(GuestPrompt.waitingText(for: waiting, activity: "Waiting for Kai", isOwner: false) == "Waiting for Kai")
    #expect(GuestPrompt.waitingText(for: waiting, activity: "Working", isOwner: false) == "Waiting for the owner")
    #expect(GuestPrompt.waitingText(for: waiting, activity: nil, isOwner: false) == "Waiting for the owner")
    // A member answers their own; an answered card shows its outcome.
    #expect(GuestPrompt.waitingText(for: card("p2", chat: "c", approver: "member"), activity: nil, isOwner: false) == nil)
    #expect(GuestPrompt.waitingText(for: card("p3", chat: "c", status: "answered"), activity: nil, isOwner: false) == nil)
    #expect(GuestPrompt.waitingText(for: card("p4", chat: "c", approver: nil), activity: nil, isOwner: false) == nil)
}

@Test func theOwnersOwnDeviceShowsTheButtonsOnACardForTheOwner() {
    // A -p run's card (kith ask, a schedule) and a bot guest's card are both approver "owner": on the owner's device
    // they are his to answer, whatever the activity line says.
    #expect(GuestPrompt.waitingText(for: card("p1", chat: "c"), activity: "Waiting for Kai", isOwner: true) == nil)
    #expect(GuestPrompt.waitingText(for: card("p2", chat: "c"), activity: nil, isOwner: true) == nil)
    #expect(GuestPrompt.waitingText(for: card("p3", chat: "c", approver: "member"), activity: nil, isOwner: true) == nil)
}

@Test func theHostsCanAnswerDecidesTheButtons() {
    var mine = card("p1", chat: "c", approver: "owner")
    mine.data.canAnswer = true
    #expect(GuestPrompt.waitingText(for: mine, activity: "Waiting for Kai", isOwner: false) == nil)
    var theirs = card("p2", chat: "c", approver: "member")
    theirs.data.canAnswer = false
    #expect(GuestPrompt.waitingText(for: theirs, activity: "Waiting for Kai", isOwner: false) == "Waiting for Kai")
    #expect(BoardModel.isWaiting(mine) && !BoardModel.isWaiting(theirs))
}

@MainActor @Test func aGuestsPromptMakesNoBotAndNoUnreadChatAndIsOnlyTheOwnersToAnswer() throws {
    let suite = "MultiMacTests.\(UUID().uuidString)"
    defer { UserDefaults(suiteName: suite)?.removePersistentDomain(forName: suite) }
    let storage = SharedStore.Context(accountID: "user_four", suite: suite)
    // The stores mirror their entries to a cache file per account and computer, outside the suite. An earlier run's
    // answered "p1" would otherwise load first and win over this run's newer-looking-or-equal upsert.
    storage.erase()
    defer { storage.erase() }
    let a = mac("A", keyByte: 1), b = mac("B", keyByte: 2)
    storage.computers = [a, b]
    let tokens = Tokens()
    tokens.invites[b.id] = StoredInvite(inviteId: "inv_1", token: fakeToken)
    let accounts = AccountStore(storage: storage, cloud: nil, inviteTokens: tokens) { computer in
        BotStore(computer: computer, storage: storage) { FakeRemote(.connecting) }
    }
    defer { accounts.retire() }
    accounts.setActive(false)
    let owned = try #require(accounts.store(for: a.id))
    owned.bots["mine"] = try bot("mine", agent: "house")
    owned.upsert(card("p1", chat: "chat-sam"))
    #expect(owned.roster.map(\.id) == ["mine"])
    #expect(owned.roster.allSatisfy { $0.unread == 0 })
    #expect(accounts.guestPrompts(for: a.id).map(\.id) == ["p1"])
    // Not another computer's, and never an invitee's own computer.
    #expect(accounts.guestPrompts(for: b.id).isEmpty)
    accounts.store(for: b.id)?.upsert(card("p9", chat: "chat-x"))
    #expect(accounts.guestPrompts(for: b.id).isEmpty)
    #expect(accounts.guestPrompts(for: a.id).map(\.id) == ["p1"])

    // Answered on another device: it leaves the list.
    owned.upsert(card("p1", chat: "chat-sam", status: "answered", at: 2))
    #expect(accounts.guestPrompts(for: a.id).isEmpty)

    // A link to that chat opens the request on a computer the account owns, and the chat otherwise.
    let ref = BotReference(accountId: accounts.accountId, computerId: a.id, botId: "chat-sam")
    #expect(accounts.opensGuestPrompt(ref))
    #expect(!accounts.opensGuestPrompt(BotReference(accountId: accounts.accountId, computerId: a.id, botId: "mine")))
    #expect(!accounts.opensGuestPrompt(BotReference(accountId: accounts.accountId, computerId: b.id, botId: "chat-sam")))
}

// MARK: - access removed

@MainActor @Test func aRevokedMemberComputerDropsWhatItShowedButAnOwnersDoesNot() throws {
    let suite = "MultiMacTests.\(UUID().uuidString)"
    defer { UserDefaults(suiteName: suite)?.removePersistentDomain(forName: suite) }
    let storage = SharedStore.Context(accountID: "user_five", suite: suite)
    let a = mac("A", keyByte: 1), b = mac("B", keyByte: 2)
    storage.computers = [a, b]
    let tokens = Tokens()
    tokens.invites[b.id] = StoredInvite(inviteId: "inv_1", token: fakeToken)
    let accounts = AccountStore(storage: storage, cloud: nil, inviteTokens: tokens) { computer in
        BotStore(computer: computer, storage: storage) { FakeRemote(.connecting) }
    }
    defer { accounts.retire() }
    accounts.setActive(false)
    for id in [a.id, b.id] {
        let store = try #require(accounts.store(for: id))
        store.bots["x"] = try bot("x", agent: "house")
        store.upsert(card("e-\(id)", chat: "x"))
        store.setConnection(.unauthorized("No access"))
    }
    #expect(accounts.store(for: b.id)?.bots.isEmpty == true)
    #expect(accounts.store(for: b.id)?.entries.isEmpty == true)
    #expect(accounts.store(for: a.id)?.bots["x"] != nil)
}
