import Foundation
import Testing
@testable import KithKit
@testable import KithUI

// Test tokens are obviously fake and low-entropy on purpose: realistic-looking secrets get flagged by scanners.
private let fakeToken = String(repeating: "a", count: 43)
private let link = InviteLink(computerId: "comp_1", inviteId: "inv_1", token: fakeToken)

private struct StoreRefused: Error {}

/// The cloud, the Keychain, the account and the computer, scripted; records the order things were done in.
@MainActor
private final class FakeDeps: InviteJoinDeps {
    var isSignedIn = true
    var signInError: Error?
    var acceptResult: Result<InviteAccepted, Error> = .success(InviteAccepted(computerId: "comp_1", membershipId: "mem_1"))
    var keepError: Error?
    var askResult: Result<Bool, Error> = .success(true)
    var connectResult: Result<FakeHost?, Error> = .success(FakeHost())
    private(set) var log: [String] = []
    private(set) var acceptedProof: String?
    private(set) var kept: [ComputerID: StoredInvite] = [:]

    func waitForSignIn() async throws {
        log.append("signIn")
        if let signInError { throw signInError }
        isSignedIn = true
    }

    func accept(inviteId: String, proof: String) async throws -> InviteAccepted {
        log.append("accept")
        acceptedProof = proof
        return try acceptResult.get()
    }

    func keep(_ invite: StoredInvite, for computer: ComputerID) throws {
        log.append("keep")
        if let keepError { throw keepError }
        kept[computer] = invite
    }

    func requestAccess(_ computer: ComputerID) async throws -> Bool {
        log.append("ask")
        return try askResult.get()
    }

    func connect(_ computer: ComputerID) async throws -> (any InviteHost)? {
        log.append("connect")
        return try connectResult.get()
    }
}

private final class FakeHost: InviteHost, @unchecked Sendable {
    var error: Error?
    private(set) var proved: [(String, String)] = []

    func proveInvite(inviteId: String, token: String) async throws {
        proved.append((inviteId, token))
        if let error { throw error }
    }
}

@MainActor
private func run(_ deps: FakeDeps) async -> (final: InviteJoinState, steps: [InviteJoinStep]) {
    var steps: [InviteJoinStep] = []
    let final = await InviteJoin.run(link, deps: deps) { steps.append($0.step) }
    return (final, steps)
}

@MainActor @Test func theJoinGoesThroughEveryStepInOrderAndProvesTheTokenToTheComputer() async {
    let deps = FakeDeps()
    let host = FakeHost()
    deps.connectResult = .success(host)
    let (final, steps) = await run(deps)
    #expect(final == InviteJoinState(.done, computerId: "comp_1"))
    #expect(steps == [.joining, .asking, .connecting, .proving, .done])
    #expect(deps.log == ["accept", "keep", "ask", "connect"])
    // The cloud got h1, never the token.
    #expect(deps.acceptedProof == InviteLink.proof(of: fakeToken))
    #expect(deps.acceptedProof != fakeToken)
    #expect(deps.kept["comp_1"] == StoredInvite(inviteId: "inv_1", token: fakeToken))
    #expect(host.proved.map(\.0) == ["inv_1"] && host.proved.map(\.1) == [fakeToken])
}

@MainActor @Test func signedOutOpensSignInFirstAndStopsIfItIsCancelled() async {
    let deps = FakeDeps()
    deps.isSignedIn = false
    let (final, steps) = await run(deps)
    #expect(steps.first == .signingIn)
    #expect(final.step == .done)
    #expect(deps.log.first == "signIn")

    let cancelled = FakeDeps()
    cancelled.isSignedIn = false
    cancelled.signInError = CancellationError()
    let (stopped, _) = await run(cancelled)
    #expect(stopped.step == .failed)
    // Nothing went to the cloud or the computer.
    #expect(cancelled.log == ["signIn"])
}

@MainActor @Test(arguments: [
    (403, "forbidden", "This is your own computer"),
    (404, "notFound", "This invite doesn't exist"),
    (409, "conflict", "This invite was already used"),
    (410, "inviteExpired", "This invite has expired"),
])
func theCloudsOwnWordsShowForEachAcceptFailure(status: Int, code: String, message: String) async {
    let deps = FakeDeps()
    deps.acceptResult = .failure(CloudError(status: status, code: code, message: message))
    let (final, steps) = await run(deps)
    #expect(final.step == .failed && final.message == message)
    #expect(steps == [.joining, .failed])
    // Nothing is kept or asked for an invite that didn't take.
    #expect(deps.log == ["accept"])
    #expect(deps.kept.isEmpty)
}

@MainActor @Test func aCloudThatCantBeReachedSaysSo() async {
    let deps = FakeDeps()
    deps.acceptResult = .failure(URLError(.notConnectedToInternet))
    let (final, _) = await run(deps)
    #expect(final.step == .failed)
    #expect(final.message?.contains("Check your connection") == true)
}

@MainActor @Test func aKeychainThatRefusesTheTokenStopsTheJoinBeforeAnythingElse() async {
    let deps = FakeDeps()
    deps.keepError = StoreRefused()
    let host = FakeHost()
    deps.connectResult = .success(host)
    let (final, steps) = await run(deps)
    #expect(final.step == .failed)
    #expect(final.message?.contains("safely") == true)
    #expect(final.message?.contains(fakeToken) == false)
    #expect(steps == [.joining, .failed])
    #expect(deps.log == ["accept", "keep"])
    #expect(host.proved.isEmpty)
}

@MainActor @Test func anOfflineComputerLeavesTheJoinWaitingAndARetryProves() async {
    let deps = FakeDeps()
    deps.connectResult = .success(nil)
    let (waiting, _) = await run(deps)
    #expect(waiting.step == .waiting && waiting.message != nil)
    // The token is already kept, so the app can prove when it next connects.
    #expect(deps.kept["comp_1"]?.token == fakeToken)

    // Opening the link again: accept answers the same membership, then the computer is there.
    let host = FakeHost()
    deps.connectResult = .success(host)
    let (done, _) = await run(deps)
    #expect(done.step == .done)
    #expect(host.proved.count == 1)
}

@MainActor @Test func aComputerOfflineAtAskingWaitsWithoutConnecting() async {
    let deps = FakeDeps()
    deps.askResult = .success(false)
    let (final, steps) = await run(deps)
    #expect(final.step == .waiting)
    #expect(steps == [.joining, .asking, .waiting])
    #expect(!deps.log.contains("connect"))
}

@MainActor @Test func aDroppedLinkWhileProvingWaitsButARefusalFails() async {
    let drop = FakeDeps()
    let flaky = FakeHost()
    flaky.error = HostError.unreachable
    drop.connectResult = .success(flaky)
    let (waiting, _) = await run(drop)
    #expect(waiting.step == .waiting)

    let refused = FakeDeps()
    let host = FakeHost()
    host.error = HostError.http(403, "That isn't this invite's token.")
    refused.connectResult = .success(host)
    let (failed, _) = await run(refused)
    #expect(failed.step == .failed && failed.message == "That isn't this invite's token.")
}

@MainActor @Test func rateLimitedProofShowsTheComputersWords() async {
    let deps = FakeDeps()
    let host = FakeHost()
    host.error = HostError.http(403, "Too many tries. Wait a few minutes.")
    deps.connectResult = .success(host)
    let (final, _) = await run(deps)
    #expect(final.step == .failed && final.message == "Too many tries. Wait a few minutes.")
}

@MainActor @Test func aDeviceThatAlreadyShowedItsInviteIsDone() async {
    let deps = FakeDeps()
    let host = FakeHost()
    host.error = HostError.http(400, "This device has already shown its invite.")
    deps.connectResult = .success(host)
    let (final, _) = await run(deps)
    #expect(final.step == .done)
}

@MainActor @Test func aComputerThatTurnsTheDeviceAwayFailsWithItsMessage() async {
    let deps = FakeDeps()
    deps.connectResult = .failure(HostError.unauthorized("This device isn't allowed on this computer."))
    let (final, _) = await run(deps)
    #expect(final.step == .failed && final.message == "This device isn't allowed on this computer.")
}

@MainActor @Test func theCloudBeingBusyWhenAskingForAccessWaits() async {
    let deps = FakeDeps()
    deps.askResult = .failure(CloudError(status: 503, code: "http503"))
    let (final, _) = await run(deps)
    #expect(final.step == .waiting)
    #expect(deps.kept["comp_1"] != nil)
}

// MARK: - the stored token is shown before anything else

private final class FakeTokens: InviteTokenStore, @unchecked Sendable {
    var invites: [ComputerID: StoredInvite] = [:]
    func stored(for computer: ComputerID) -> StoredInvite? { invites[computer] }
    func keep(_ invite: StoredInvite, for computer: ComputerID) throws { invites[computer] = invite }
    func forget(_ computer: ComputerID) { invites[computer] = nil }
}

private func memberComputer() -> Computer {
    let key = Data(repeating: 7, count: 32)
    let encoded = key.base64EncodedString().replacingOccurrences(of: "+", with: "-")
        .replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
    return Computer(id: RelayCrypto.computerId(signKey: key), name: "Owner's Mac", signKey: encoded, cloud: URL(string: "https://cloud.example.dev"))
}

@MainActor @Test func aMemberComputersFirstCallIsTheProofAndEveryReconnectProvesAgain() async throws {
    let suite = "InviteJoinTests.\(UUID().uuidString)"
    defer { UserDefaults(suiteName: suite)?.removePersistentDomain(forName: suite) }
    let storage = SharedStore.Context(accountID: "user_member", suite: suite)
    let computer = memberComputer()
    storage.computers = [computer]
    let tokens = FakeTokens()
    tokens.invites[computer.id] = StoredInvite(inviteId: "inv_1", token: fakeToken)
    let fake = FakeRemote(.ready(.direct))
    let accounts = AccountStore(storage: storage, cloud: nil, inviteTokens: tokens) { c in
        BotStore(computer: c, storage: storage) { fake }
    }
    defer { accounts.retire() }
    accounts.setActive(true)

    var seen: [String] = []
    for _ in 0..<200 {
        seen = await fake.calls
        if seen.contains("hello") { break }
        try await Task.sleep(for: .milliseconds(10))
    }
    #expect(seen.first == "proveInvite")
    #expect(seen.contains("hello"))
    #expect(accounts.role(for: computer.id) == .member)
    #expect(accounts.ownedComputers.isEmpty)

    // Forgetting the computer drops the token with it.
    accounts.forget(computer.id)
    #expect(tokens.invites.isEmpty)
}

@MainActor @Test func anOwnersComputerNeverSendsAProof() async throws {
    let suite = "InviteJoinTests.\(UUID().uuidString)"
    defer { UserDefaults(suiteName: suite)?.removePersistentDomain(forName: suite) }
    let storage = SharedStore.Context(accountID: "user_owner", suite: suite)
    let computer = memberComputer()
    storage.computers = [computer]
    let fake = FakeRemote(.ready(.direct))
    let accounts = AccountStore(storage: storage, cloud: nil, inviteTokens: FakeTokens()) { c in
        BotStore(computer: c, storage: storage) { fake }
    }
    defer { accounts.retire() }
    accounts.setActive(true)
    var seen: [String] = []
    for _ in 0..<200 {
        seen = await fake.calls
        if seen.contains("hello") { break }
        try await Task.sleep(for: .milliseconds(10))
    }
    #expect(seen.contains("hello") && !seen.contains("proveInvite"))
    #expect(accounts.role(for: computer.id) == .owner)
}

// MARK: - the People store

private final class FakePeopleHost: PeopleHost, @unchecked Sendable {
    var peopleResult: Result<[HostPerson], Error> = .success([HostPerson(personId: "sam", name: "Sam", agents: ["house": .guest])])
    var created: [(String?, String?, [String: InviteRole])] = []
    var createDelay: Duration = .zero
    var revoked: [String] = []
    var removed: [String] = []
    var restored: [String] = []
    var restoreFailure: Error?

    func people() async throws -> [HostPerson] { try peopleResult.get() }
    func invites() async throws -> [HostInvite] {
        try peopleResult.get()
        return [HostInvite(inviteId: "inv_open", personId: "sam", agents: [:], createdAt: 0, expiresAt: 9_999_999_999_999, usedAt: nil, revokedAt: nil),
                HostInvite(inviteId: "inv_old", personId: "sam", agents: [:], createdAt: 0, expiresAt: 5, usedAt: nil, revokedAt: nil)]
    }
    func createInvite(name: String?, personId: String?, agents: [String: InviteRole]) async throws -> CreatedInvite {
        created.append((name, personId, agents))
        try await Task.sleep(for: createDelay)
        return CreatedInvite(inviteId: "inv_new", link: "kith://invite?c=comp_1&i=inv_new&t=\(fakeToken)", expiresAt: 9_999_999_999_999, personId: "sam")
    }
    func revokeInvite(_ inviteId: String) async throws { revoked.append(inviteId) }
    func setPersonAccess(personId: String, agents: [String: InviteRole]?, projects: [String]?) async throws {}
    func revokePerson(_ personId: String) async throws { removed.append(personId) }
    func unrevokePerson(_ personId: String) async throws {
        if let failure = restoreFailure { throw failure }
        restored.append(personId)
    }
    func audit(since: Date?, personId: String?) async throws -> [AuditRow] { [] }
    func projectList() async throws -> [HQProject] { [] }
}

@MainActor @Test func peopleLoadsAndOnlyOpenInvitesArePending() async {
    let host = FakePeopleHost()
    let store = PeopleStore { host }
    await store.load()
    #expect(store.people.map(\.personId) == ["sam"])
    #expect(store.pending.map(\.inviteId) == ["inv_open"])
    #expect(!store.unsupported && store.loaded)
}

@MainActor @Test func anOlderHostHidesPeople() async {
    let host = FakePeopleHost()
    host.peopleResult = .failure(HostError.http(404, "unknown method `people`"))
    let store = PeopleStore { host }
    await store.load()
    #expect(store.unsupported)
    #expect(store.error == nil)
}

@MainActor @Test func aDoubleTapOnCreateMakesOneInvite() async {
    let host = FakePeopleHost()
    host.createDelay = .milliseconds(100)
    let store = PeopleStore { host }
    async let first = store.createInvite(name: "Sam", personId: nil, agents: ["house": .guest])
    try? await Task.sleep(for: .milliseconds(20))
    #expect(store.busy)
    let second = await store.createInvite(name: "Sam", personId: nil, agents: ["house": .guest])
    #expect(second == nil)
    #expect(await first?.inviteId == "inv_new")
    #expect(host.created.count == 1)
    #expect(!store.busy)
}

@MainActor @Test func hostRefusalsShowTheHostsWords() async {
    final class Refusing: PeopleHost, @unchecked Sendable {
        func people() async throws -> [HostPerson] { [] }
        func invites() async throws -> [HostInvite] { [] }
        func createInvite(name: String?, personId: String?, agents: [String: InviteRole]) async throws -> CreatedInvite {
            throw HostError.http(400, "Sign in to a Kith account on this computer first: invites need it to be claimed.")
        }
        func revokeInvite(_ inviteId: String) async throws {}
        func setPersonAccess(personId: String, agents: [String: InviteRole]?, projects: [String]?) async throws {}
        func revokePerson(_ personId: String) async throws {}
        func unrevokePerson(_ personId: String) async throws {}
        func audit(since: Date?, personId: String?) async throws -> [AuditRow] { [] }
        func projectList() async throws -> [HQProject] { [] }
    }
    let store = PeopleStore { Refusing() }
    let made = await store.createInvite(name: "Sam", personId: nil, agents: ["house": .guest])
    #expect(made == nil)
    #expect(store.error?.hasPrefix("Sign in to a Kith account") == true)
}

@MainActor @Test func removingAPersonAndRevokingAnInviteReachTheHost() async {
    let host = FakePeopleHost()
    let store = PeopleStore { host }
    await store.revokeInvite("inv_open")
    #expect(await store.remove("sam"))
    #expect(host.revoked == ["inv_open"] && host.removed == ["sam"])
}

@MainActor @Test func undoingARemoveRestoresThePersonAndAFailureShowsTheHostsReason() async {
    struct Refused: LocalizedError { var errorDescription: String? { "Sam was already deleted." } }
    let host = FakePeopleHost()
    let store = PeopleStore { host }
    #expect(store.removed == nil)
    await store.load()
    #expect(await store.remove("sam"))
    #expect(store.removed?.name == "Sam")
    host.restoreFailure = Refused()
    await store.restore("sam")
    #expect(host.restored.isEmpty && store.error == "Sam was already deleted." && store.removed?.personId == "sam")
    host.restoreFailure = nil
    await store.restore("sam")
    #expect(host.restored == ["sam"] && store.removed == nil && store.error == nil)
}
