import KithKit
import KithUI
import SwiftUI

/// Share one agent: who has it, at which level, and the Share flow already pointed at it.
struct ShareAgentSheet: View {
    let store: BotStore
    let agent: String
    let name: String
    @State private var service: HostSharingService
    @State private var created: CreatedInvite?
    @State private var removing: SharingPerson?
    @State private var sharing = false
    @State private var botSheet: BotSheetTarget?
    /// What each holder spent here this month and their limit (owner only; nothing shows without it).
    @State private var spend: PeopleSpend

    /// `people` and `created` are for the DEBUG demo screens; the app passes neither.
    init(store: BotStore, agent: String, name: String, people: PeopleStore? = nil, created: CreatedInvite? = nil) {
        self.store = store
        self.agent = agent
        self.name = name
        let people = people ?? PeopleStore(store: store)
        _service = State(initialValue: HostSharingService(people: people, agents: { agentsOffered(on: store, plus: [agent]) }))
        _created = State(initialValue: created)
        _spend = State(initialValue: PeopleSpend(store: store, agent: agent))
    }

    private var holders: [(person: SharingPerson, share: Share)] { service.holders(of: agent) }

    var body: some View {
        Group {
            if let created {
                InviteLinkView(invite: created, name: service.person(created.personId ?? "")?.name ?? "")
            } else {
                form
            }
        }
        .background(Palette.background)
        .safeAreaInset(edge: .top, spacing: 0) { ModalHeader("Share \(name)") }
        .task { await service.reload() }
        .task { await spend.load() }
        .kithSheet(isPresented: $sharing) { InviteSheet(store: store, people: service.store, prefill: .agent(agent)) }
        .kithDialog("Remove \(removing?.name ?? "")?",
                    isPresented: Binding(get: { removing != nil }, set: { if !$0 { removing = nil } }),
                    message: "They lose \(name). Anything else they have stays.") {
            guard let person = removing else { return [] }
            return [DialogAction("Remove", destructive: true) {
                Task { _ = await service.unshare(.agent(agent), from: person.id) }
            }]
        }
    }

    private var form: some View {
        CardForm {
            CardSection("Shared with", footer: holders.isEmpty ? "No one has \(name) yet." : nil) {
                if spend.people != nil { OwnerSpendRow(spend: spend) }
                ForEach(holders, id: \.person.id) { holder in row(holder.person, holder.share) }
            }
            PeopleLimits(spend: spend, holders: holders.map { (id: $0.person.id, name: $0.person.name) })
            if let error = spend.error {
                Text(error).font(.footnote).foregroundStyle(Palette.danger)
            }
            if let error = service.error {
                Text(error).font(.footnote).foregroundStyle(Palette.danger)
            }
            Button { sharing = true } label: { Text("Share \(name)").frame(maxWidth: .infinity, minHeight: 28) }
                .buttonStyle(.primary)
                .disabled(service.busy)

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

    private func change(_ level: ShareLevel, for person: String) {
        Task { _ = await service.setLevel(level, of: agent, for: person) }
    }

    private func row(_ person: SharingPerson, _ share: Share) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 12) {
                SkyPatch(id: person.id, size: 30)
                VStack(alignment: .leading, spacing: 2) {
                    Text(person.name).font(.body.weight(.semibold))
                    Text([spend.line(for: person.id), share.everyone ? "Everyone" : nil, person.status.title].compactMap { $0 }.joined(separator: " · "))
                        .font(.subheadline).foregroundStyle(Palette.secondary)
                    PersonSpendBar(spend: spend, person: person.id)
                }
                Spacer()
                // Everyone's agents are changed for everyone, not per person.
                if !share.everyone {
                    Button("Remove") { removing = person }
                        .buttonStyle(.plain).foregroundStyle(Palette.danger).disabled(service.busy)
                }
            }
            if let level = share.level {
                if share.everyone {
                    Text(level.title).font(.subheadline).foregroundStyle(Palette.secondary)
                } else {
                    SegmentedChoice(selection: Binding(get: { level }, set: { change($0, for: person.id) }), options: ShareLevel.allCases.map { ($0, $0.title) })
                    .disabled(service.busy)
                }
            }
            ForEach(person.openInvites, id: \.self) { invite in
                Button("Cancel invite") { Task { await service.cancelInvite(invite) } }
                    .buttonStyle(.plain).font(.subheadline).foregroundStyle(Palette.danger).disabled(service.busy)
            }
        }
    }
}
