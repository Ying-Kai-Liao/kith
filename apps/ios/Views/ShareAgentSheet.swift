import KithKit
import KithUI
import SwiftUI

/// Share one agent: who has it, and the way to invite someone to it. Same wording and steps as the desktop sheet.
struct ShareAgentSheet: View {
    let store: BotStore
    let agent: String
    let name: String
    @State private var people: PeopleStore
    /// Nil is a new person.
    @State private var personId: String?
    @State private var newName = ""
    @State private var role: InviteRole = .guest
    @State private var created: CreatedInvite?
    @State private var removing: HostPerson?
    @State private var botSheet: BotSheetTarget?
    /// What each holder spent here this month and their limit (owner only; nothing shows without it).
    @State private var spend: PeopleSpend

    /// `people` and `created` are for the DEBUG demo screens; the app passes neither.
    init(store: BotStore, agent: String, name: String, people: PeopleStore? = nil, created: CreatedInvite? = nil) {
        self.store = store
        self.agent = agent
        self.name = name
        _people = State(initialValue: people ?? PeopleStore(store: store))
        _created = State(initialValue: created)
        _spend = State(initialValue: PeopleSpend(store: store, agent: agent))
    }

    private var shared: [(person: HostPerson, role: InviteRole)] { AgentShare.sharedWith(agent, in: people.people) }
    private var openInvites: [HostInvite] { AgentShare.openInvites(for: agent, in: people.invites) }
    /// One row per person: the host grants the membership when it makes the invite, so a new invitee is a holder
    /// already. Invite rows are only for people who don't hold the agent.
    private var waiting: [HostInvite] {
        let holders = Set(shared.map(\.person.personId))
        return openInvites.filter { !holders.contains($0.personId) }
    }
    private func state(of person: HostPerson) -> String {
        if openInvites.contains(where: { $0.personId == person.personId }) { return "Invite not used yet" }
        return person.joined ? "Joined" : "Waiting to join"
    }
    /// Everyone who doesn't already have the agent.
    private var whoOptions: [(id: String, label: String)] {
        [(id: "", label: "New person")] + people.people.filter { $0.agents[agent] == nil }.map { (id: $0.personId, label: $0.name) }
    }
    private var canCreate: Bool {
        !people.busy && (personId != nil || !newName.trimmingCharacters(in: .whitespaces).isEmpty)
    }

    var body: some View {
        Group {
            if let created {
                InviteLinkView(invite: created, name: people.person(created.personId ?? "")?.name ?? newName)
            } else {
                form
            }
        }
        .background(Palette.background)
        .safeAreaInset(edge: .top, spacing: 0) { ModalHeader("Share access to \(name)") }
        .task { await people.load() }
        .task { await spend.load() }
        .kithDialog("Remove \(removing?.name ?? "")?",
                    isPresented: Binding(get: { removing != nil }, set: { if !$0 { removing = nil } }),
                    message: "They lose \(name). Anything else they have stays.") {
            guard let person = removing else { return [] }
            return [DialogAction("Remove", destructive: true) {
                Task { await people.removeAgent(agent, from: person) }
            }]
        }
    }

    private var form: some View {
        CardForm {
            CardSection("Shared with", footer: shared.isEmpty && waiting.isEmpty ? "No one has \(name) yet. Invite someone below." : nil) {
                if spend.people != nil { OwnerSpendRow(spend: spend) }
                ForEach(shared, id: \.person.personId) { row in
                    HStack(spacing: 12) {
                        SkyPatch(id: row.person.personId, size: 30)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(row.person.name).font(.body.weight(.semibold))
                            Text([spend.line(for: row.person.personId), row.role.title, state(of: row.person)].compactMap { $0 }.joined(separator: " · "))
                                .font(.subheadline).foregroundStyle(Palette.secondary)
                            PersonSpendBar(spend: spend, person: row.person.personId)
                        }
                        Spacer()
                        Button("Remove") { removing = row.person }
                            .buttonStyle(.plain)
                            .foregroundStyle(Palette.danger)
                            .disabled(people.busy)
                    }
                }
                ForEach(waiting) { invite in
                    HStack(spacing: 12) {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(people.person(invite.personId)?.name ?? invite.personId).font(.body.weight(.semibold))
                            Text("\(invite.agents[agent]?.title ?? "Guest") · Invite expires \(invite.expiry.formatted(.relative(presentation: .named)))")
                                .font(.subheadline).foregroundStyle(Palette.secondary)
                        }
                        Spacer()
                        Button("Cancel") { Task { await people.revokeInvite(invite.inviteId) } }
                            .buttonStyle(.plain)
                            .foregroundStyle(Palette.danger)
                            .disabled(people.busy)
                    }
                }
            }
            PeopleLimits(spend: spend, holders: shared.map { (id: $0.person.personId, name: $0.person.name) })
            if let error = spend.error {
                Text(error).font(.footnote).foregroundStyle(Palette.danger)
            }
            CardSection("Invite someone", footer: "A member can do more with an agent than a guest. Guest is the safer start. They get \(name) only.") {
                if whoOptions.count > 1 {
                    ChoicePicker(selection: Binding(get: { personId ?? "" }, set: { personId = $0.isEmpty ? nil : $0 }),
                                 options: whoOptions)
                }
                if personId == nil {
                    TextField("Name", text: $newName)
                        .textInputAutocapitalization(.words)
                        .submitLabel(.done)
                }
                SegmentedChoice(selection: $role, options: [(InviteRole.guest, "Guest"), (InviteRole.member, "Member")])
            }
            if let error = people.error {
                Text(error).font(.footnote).foregroundStyle(Palette.danger)
            }
            Button {
                Task { await create() }
            } label: {
                ZStack {
                    Text("Create invite").opacity(people.busy ? 0 : 1)
                    if people.busy { Spinner(size: 18) }
                }
                .frame(maxWidth: .infinity, minHeight: 28)
            }
            .buttonStyle(.primary)
            .disabled(!canCreate)

            if !store.botGuests.unsupported {
                CardSection("Bots", footer: "A bot from Grok or another app can ask \(name) for you. It never acts as you.") {
                    Button { botSheet = .invite(on: store, preset: [agent]) } label: {
                        LinkRow { Label("Invite a bot", systemImage: "bubble.left.and.bubble.right") }
                    }
                    .buttonStyle(.plain)
                }
            }
        }
        .botSheets($botSheet, viewer: .owner)
    }

    private func create() async {
        let typed = newName.trimmingCharacters(in: .whitespaces)
        let body = AgentShare.inviteAgents(agent, role: role)
        if let made = await people.createInvite(name: personId == nil ? typed : nil, personId: personId, agents: body) {
            created = made
        }
    }
}
