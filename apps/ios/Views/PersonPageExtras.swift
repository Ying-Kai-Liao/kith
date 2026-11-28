import KithKit
import KithUI
import SwiftUI

/// What the old edit sheet also did and the person page keeps: the rooms shared with them, and their bots.
struct PersonRoomsSection: View {
    let person: HostPerson
    let people: PeopleStore
    let disabled: Bool

    /// Projects the host listed, plus any already shared that it no longer lists.
    private var choices: [HQProject] {
        let known = Set(people.projects.map(\.id))
        return people.projects + Set(person.projects).subtracting(known).sorted().map { HQProject(id: $0) }
    }

    private func set(_ id: String, on: Bool) {
        var rooms = Set(person.projects)
        if on { rooms.insert(id) } else { rooms.remove(id) }
        Task { _ = await people.setAccess(person.personId, agents: person.agents, projects: rooms.sorted()) }
    }

    var body: some View {
        if !choices.isEmpty {
            CardSection("Shared rooms", footer: "Only the rooms you tick are shared with \(person.name).") {
                ForEach(choices, id: \.id) { project in
                    Toggle(project.id, isOn: Binding(get: { person.projects.contains(project.id) }, set: { set(project.id, on: $0) }))
                        .toggleStyle(.kith)
                        .disabled(disabled)
                }
            }
        }
    }
}

/// "Always allow Sam's bots" and the bots they have. Nothing shows on a host without bot guests.
struct PersonBotsSection: View {
    let person: HostPerson
    let people: PeopleStore
    let store: BotStore
    /// What the switch was just set to, until the people list says so.
    @State private var autoBots: Bool?

    var body: some View {
        if !store.botGuests.unsupported {
            let theirs = BotGuestStore.live(store.botGuests.bots(of: person.personId))
            CardSection("\(person.name)'s bots", footer: "New bots \(person.name) invites start without waiting for you. Turning this on also allows any waiting now. Pausing \(person.name) stops these too.") {
                Toggle("Always allow \(person.name)'s bots", isOn: Binding(
                    get: { autoBots ?? person.autoBots },
                    set: { on in
                        autoBots = on
                        Task {
                            let done = await store.botGuests.setAutoAllow(personId: person.personId, on: on)
                            await people.load()
                            if !done { autoBots = nil }
                        }
                    }))
                    .toggleStyle(.kith)
                    .disabled(store.botGuests.busy || store.isOffline)
                ForEach(theirs) { bot in
                    BotGuestRow(bot: bot, viewer: .owner, store: store, indent: 0, open: { _ in }, newLink: { _ in })
                }
                if theirs.isEmpty {
                    Text("No bots yet").font(.subheadline).foregroundStyle(Palette.secondary)
                }
            }
        }
    }
}

#if DEBUG
/// The `-kithDemoScreens person` and `person-paused` shots: Sam's page over a fixed People list, no host.
struct DemoPersonPage: View {
    let store: BotStore
    @State private var people: PeopleStore

    init(store: BotStore, paused: Bool) {
        self.store = store
        _people = State(initialValue: PeopleStore.demo(paused: paused))
    }

    var body: some View { PersonPageView(personId: "sam", people: people, store: store, memoryDemo: [
        "ada": ["preferences.md", "trip-plans.md", "recipes.md"], "remy": []]) }
}
#endif
