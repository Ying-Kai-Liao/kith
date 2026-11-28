import KithKit
import KithUI
import SwiftUI

/// One person: each agent they hold with its level and monthly limit, and Pause (a suspend the owner can undo).
/// Opened from the Sharing screen and the People screen. Chat with the person is a later phase.
struct PersonPageView: View {
    let personId: String
    let people: PeopleStore
    let store: BotStore
    /// Debug demos: made-up memory files per agent, no host.
    var memoryDemo: [String: [String]]?
    @State private var confirm: Confirm?
    @State private var working = false

    private enum Confirm: Identifiable {
        case pause, remove
        var id: Self { self }
    }

    private var person: HostPerson? { people.person(personId) }
    private var disabled: Bool { people.busy || working || store.isOffline }

    var body: some View {
        Group {
            if let person {
                CardForm {
                    header(person)
                    if store.isOffline {
                        Text("\(store.hostName) is offline. You can change this person when it is back.")
                            .font(.footnote).foregroundStyle(Palette.secondary)
                    }
                    if let error = people.error {
                        Text(error).font(.footnote).foregroundStyle(Palette.danger)
                    }
                    switch PersonState(person) {
                    case .invited:
                        invited
                        removeSection(person)
                    case .paused:
                        paused(person)
                        memory(person)
                    case .active:
                        agentsSection(person)
                        memory(person)
                        PersonRoomsSection(person: person, people: people, disabled: disabled)
                        PersonBotsSection(person: person, people: people, store: store)
                        pauseSection(person)
                        removeSection(person)
                    }
                }
            } else {
                Color.clear
            }
        }
        .background(Palette.background)
        .safeAreaInset(edge: .top, spacing: 0) { ModalHeader(person?.name ?? "") }
        .refreshable { await people.load() }
        // Reload on appear: another device may have changed them.
        .task { await people.load(); await people.loadProjects(); await store.refreshAllRoomPeople() }
        .kithDialog("Pause \(person?.name ?? "")?", isPresented: bound(.pause), message: effect(of: person)) {
            [DialogAction("Pause", destructive: true) { run { await people.remove(personId) } }]
        }
        .kithDialog("Remove \(person?.name ?? "")?", isPresented: bound(.remove),
                    message: "Removes their access. You can undo this. " + (effect(of: person) ?? "")) {
            [DialogAction("Remove", destructive: true) { run { await people.remove(personId) } }]
        }
    }

    private func bound(_ kind: Confirm) -> Binding<Bool> {
        Binding(get: { confirm == kind }, set: { if !$0, confirm == kind { confirm = nil } })
    }

    /// Pausing also cuts off their bots and room seats; the confirm says so.
    private func effect(of person: HostPerson?) -> String? {
        guard let person else { return nil }
        return ["They lose access to this computer, with their bots and room seats. Their chats and memory stay.",
                RoomPeopleLogic.revokeLine(person: person.name, rooms: store.rooms(of: person.personId).map(\.name))]
            .compactMap { $0 }.joined(separator: " ")
    }

    /// A tap runs once: the buttons wait while it is in flight.
    private func run(_ work: @escaping () async -> Void) {
        guard !working else { return }
        working = true
        Task {
            await work()
            working = false
        }
    }

    private func header(_ person: HostPerson) -> some View {
        let state = PersonState(person)
        return HStack(spacing: 12) {
            Image(systemName: "person.crop.circle.fill").font(.system(size: 40)).foregroundStyle(Palette.secondary)
            VStack(alignment: .leading, spacing: 2) {
                Text(person.name).font(.title3.weight(.semibold))
                Text(state == .paused ? "Paused" : state == .invited ? "Invited, not joined" : "Active")
                    .font(.subheadline).foregroundStyle(state == .paused ? Palette.danger : Palette.secondary)
            }
            Spacer()
        }
        .accessibilityElement(children: .combine)
    }

    private var invited: some View {
        CardSection(footer: "They have not accepted yet. Cancel their link from Sharing if you changed your mind.") {
            Text("Invited, not joined").foregroundStyle(Palette.text)
        }
    }

    private func agentsSection(_ person: HostPerson) -> some View {
        let held = person.agents.keys.sorted()
        return CardSection("Agents", footer: held.isEmpty ? "No agents yet. Share one from the Sharing screen." : nil) {
            ForEach(held, id: \.self) { agent in
                PersonAgentCard(person: person, agent: agent, people: people, store: store, disabled: disabled)
            }
        }
    }

    /// What each agent keeps about them: still there, and still forgettable, while they are paused.
    private func memory(_ person: HostPerson) -> some View {
        PersonMemorySection(personName: person.name, personId: person.personId, agents: person.agents.keys.sorted(),
                            store: store, demo: memoryDemo)
    }

    private func pauseSection(_ person: HostPerson) -> some View {
        CardSection(footer: "Pause stops their access, their bots and room seats. Resume brings it back.") {
            Button { confirm = .pause } label: {
                Label("Pause \(person.name)", systemImage: "pause.circle")
                    .foregroundStyle(Palette.text)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .disabled(disabled)
        }
    }

    private func paused(_ person: HostPerson) -> some View {
        CardSection(footer: "\(person.name) can't use this computer, and neither can their bots. Their chats and memory stay.") {
            Button { run { await people.restore(personId) } } label: {
                Label("Resume \(person.name)", systemImage: "play.circle")
                    .foregroundStyle(Palette.accent)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .disabled(disabled)
        }
    }

    private func removeSection(_ person: HostPerson) -> some View {
        CardSection(footer: "Removes their access. You can undo this.") {
            Button { confirm = .remove } label: {
                Label("Remove \(person.name)", systemImage: "person.badge.minus")
                    .foregroundStyle(Palette.danger)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .disabled(disabled)
        }
    }
}

/// One agent the person holds: its level, and their monthly limit on it.
private struct PersonAgentCard: View {
    let person: HostPerson
    let agent: String
    let people: PeopleStore
    let disabled: Bool
    @State private var spend: PeopleSpend

    init(person: HostPerson, agent: String, people: PeopleStore, store: BotStore, disabled: Bool) {
        self.person = person
        self.agent = agent
        self.people = people
        self.disabled = disabled
        _spend = State(initialValue: PeopleSpend(store: store, agent: agent))
    }

    private var role: InviteRole { person.agents[agent] ?? .guest }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(agent).font(.body.weight(.semibold)).foregroundStyle(Palette.text)
            SegmentedChoice(selection: Binding(get: { role }, set: { set($0) }),
                            options: [InviteRole.guest, .member].map { ($0, $0.levelTitle) })
                .disabled(disabled)
            Text(role.levelMeaning).font(.footnote).foregroundStyle(Palette.secondary)
            PersonAgentLimit(spend: spend, personId: person.personId, name: person.name)
        }
        .task { await spend.load() }
    }

    private func set(_ new: InviteRole) {
        guard new != role else { return }
        var agents = person.agents
        agents[agent] = new
        Task { _ = await people.setAccess(person.personId, agents: agents, projects: person.projects) }
    }
}
