import KithKit
import KithUI
import SwiftUI

/// The people store of each computer this account owns, for the person rows of Crew. A computer joined by invite has
/// none (the host answers `people` only to its owner), so a guest account has no person rows and no People chip.
@MainActor @Observable
final class CrewPeople {
    private var services: [ComputerID: HostSharingService] = [:]

    /// Starts a store for each owned computer and loads the ones that are online.
    func load(_ stores: [BotStore]) async {
        for store in stores where services[store.computer.id] == nil {
            #if DEBUG
            let people = DemoScreens.active ? PeopleStore(host: { CrewDemoPeopleHost() }) : PeopleStore(store: store)
            #else
            let people = PeopleStore(store: store)
            #endif
            services[store.computer.id] = HostSharingService(people: people, agents: { agentsOffered(on: store) })
        }
        let ids = Set(stores.map(\.computer.id))
        services = services.filter { ids.contains($0.key) }
        for store in stores where DemoScreens.active || (store.connection == .online && store.mismatch == nil) {
            await services[store.computer.id]?.store.load()
        }
    }

    func service(for computerId: ComputerID) -> HostSharingService? { services[computerId] }

    /// Whether any owned computer can list people (an older host answers "unknown method").
    var available: Bool { services.values.contains { !$0.unsupported } }

    /// Person rows from every computer that lists people. Pending invites are not rows.
    func rows(shown: Set<ComputerID>) -> [CrewPerson] {
        CrewPerson.rows(services.filter { shown.contains($0.key) && !$0.value.unsupported }
            .sorted { $0.key < $1.key }.map { ($0.key, $0.value.store.people) })
    }
}

/// What tapping a person row opens. For now the Sharing screen's person sheet; the person page swaps in here.
struct CrewPersonTarget: Identifiable {
    let computerId: ComputerID
    let personId: String
    var id: String { CrewPerson.rowId(computerId: computerId, personId: personId) }
}

struct CrewPersonDestination: View {
    let target: CrewPersonTarget
    let people: CrewPeople
    @Environment(AccountStore.self) private var accounts

    var body: some View {
        if let store = accounts.store(for: target.computerId), let service = people.service(for: target.computerId) {
            PersonPageView(personId: target.personId, people: service.store, store: store)
        }
    }
}

#if DEBUG
/// The `crew-people` demo: made-up people, one of them paused, one invited and not joined yet.
private struct CrewDemoPeopleHost: PeopleHost {
    func people() async throws -> [HostPerson] {
        let now = Int64(Date().timeIntervalSince1970 * 1000)
        let seen = { (ago: Int64) in [try? JSONDecoder().decode(HostPersonDevice.self, from: Data("{\"key\":\"d\",\"lastSeenAt\":\(now - ago)}".utf8))].compactMap { $0 } }
        return [HostPerson(personId: "sam", name: "Sam", agents: ["ada": .member, "remy": .guest], joined: true, devices: seen(3_600_000)),
                HostPerson(personId: "maya", name: "Maya", agents: ["ada": .guest], joined: true, devices: seen(86_400_000 * 3)),
                HostPerson(personId: "jo", name: "Jo", agents: ["remy": .guest], joined: true, revoked: true),
                HostPerson(personId: "lee", name: "Lee", joined: false, openInvites: 1)]
    }
    func invites() async throws -> [HostInvite] { [] }
    func createInvite(name: String?, personId: String?, agents: [String: InviteRole]) async throws -> CreatedInvite { throw CancellationError() }
    func revokeInvite(_ inviteId: String) async throws {}
    func setPersonAccess(personId: String, agents: [String: InviteRole]?, projects: [String]?) async throws {}
    func revokePerson(_ personId: String) async throws {}
    func unrevokePerson(_ personId: String) async throws {}
    func audit(since: Date?, personId: String?) async throws -> [AuditRow] { [] }
    func projectList() async throws -> [HQProject] { [] }
}
#endif
