import KithKit
import SwiftUI
import UIKit

/// "Invite someone new" from a room's Add (SPEC §3): the room's agents on as guest and the room ticked; "Invite again"
/// opens it with the person picked. Kai can change any switch. Rooms never switch agents on by themselves, and
/// switching a needed agent off turns its room off.
struct RoomInviteSheet: View {
    @Environment(BotStore.self) private var model
    let group: Bot
    let agents: [RoomAgent]
    let people: PeopleStore
    let personId: String?
    @State private var name = ""
    @State private var roles: [String: InviteRole]
    @State private var ticked: [String]
    @State private var created: CreatedInvite?
    @State private var copied = false

    init(group: Bot, agents: [RoomAgent], people: PeopleStore, personId: String?) {
        self.group = group
        self.agents = agents
        self.people = people
        self.personId = personId
        let start = RoomPeopleLogic.roomInviteStart(agents: agents, room: group.id)
        let held = personId.flatMap { id in people.person(id)?.agents } ?? [:]
        _roles = State(initialValue: held.merging(start.roles) { _, new in new })
        _ticked = State(initialValue: start.rooms)
    }

    private var offered: [RoomAgent] {
        let seen = Set(agents.map(\.id))
        let others = model.roster.compactMap { b -> RoomAgent? in b.hqAgent.map { RoomAgent(id: $0, name: b.name) } }.filter { !seen.contains($0.id) }
        var unique: [RoomAgent] = []
        for a in agents + others where !unique.contains(where: { $0.id == a.id }) { unique.append(a) }
        return unique
    }

    private var rooms: [(id: String, name: String, agents: [RoomAgent])] { model.inviteRooms }
    private var who: String { personId.flatMap { people.person($0)?.name } ?? name.trimmingCharacters(in: .whitespaces) }
    private var canCreate: Bool { !roles.isEmpty && !people.busy && !who.isEmpty }

    var body: some View {
        VStack(spacing: 0) {
            ModalHeader(created == nil ? "Invite someone" : "Invite ready") {}
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    if let created { link(created) } else { form }
                }
                .padding(.horizontal, 20).padding(.bottom, 24)
            }
        }
        .background(Palette.background)
    }

    @ViewBuilder private var form: some View {
        if personId == nil {
            TextField("Name", text: $name).textInputAutocapitalization(.words).submitLabel(.done)
                .padding(14).background(Palette.surface, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        }
        Text("WHAT THEY CAN USE").font(.system(size: 13, weight: .semibold)).tracking(0.8).foregroundStyle(Palette.muted)
        ForEach(offered) { agent in
            VStack(alignment: .leading, spacing: 8) {
                Toggle(agent.name, isOn: Binding(get: { roles[agent.id] != nil }, set: { on in
                    if on { roles[agent.id] = roles[agent.id] ?? .guest } else { roles[agent.id] = nil }
                    ticked = RoomPeopleLogic.followAgents(ticked: ticked, rooms: rooms.map { ($0.id, $0.agents) }, roles: roles)
                })).toggleStyle(.kith)
                if let role = roles[agent.id] {
                    Picker(agent.name, selection: Binding(get: { role }, set: { roles[agent.id] = $0 })) {
                        ForEach(InviteRole.allCases, id: \.self) { Text($0.title).tag($0) }
                    }
                    .pickerStyle(.segmented)
                }
            }
        }
        if !rooms.isEmpty {
            Text("ROOMS").font(.system(size: 13, weight: .semibold)).tracking(0.8).foregroundStyle(Palette.muted)
            ForEach(rooms, id: \.id) { room in
                let needs = RoomPeopleLogic.roomNeeds(room.agents, roles: roles)
                Toggle(needs.isEmpty ? room.name : "\(room.name) · Needs \(RoomPeopleLogic.joined(needs.map(\.name)))",
                       isOn: Binding(get: { ticked.contains(room.id) && needs.isEmpty }, set: { on in
                           ticked.removeAll { $0 == room.id }
                           if on { ticked.append(room.id) }
                       }))
                    .toggleStyle(.kith).disabled(!needs.isEmpty)
            }
        }
        if let error = people.error { Text(error).font(.footnote).foregroundStyle(Palette.danger) }
        Button {
            Task {
                let followed = RoomPeopleLogic.followAgents(ticked: ticked, rooms: rooms.map { ($0.id, $0.agents) }, roles: roles)
                created = await people.createInvite(name: personId == nil ? who : nil, personId: personId, agents: roles, rooms: followed)
            }
        } label: {
            ZStack { Text("Create invite").opacity(people.busy ? 0 : 1); if people.busy { Spinner(size: 18) } }
                .font(.system(size: 17, weight: .semibold)).foregroundStyle(Palette.background)
                .frame(maxWidth: .infinity, minHeight: 48).background(Palette.ink, in: Capsule())
        }
        .buttonStyle(.plain).disabled(!canCreate).opacity(canCreate ? 1 : 0.4)
    }

    @ViewBuilder private func link(_ invite: CreatedInvite) -> some View {
        Text(invite.link).font(.system(.footnote, design: .monospaced)).foregroundStyle(Palette.ink).textSelection(.enabled).lineLimit(4).truncationMode(.middle)
        if let line = RoomPeopleLogic.inviteJoinsLine(name: people.person(invite.personId ?? "")?.name ?? who, rooms: ticked.compactMap { id in rooms.first { $0.id == id }?.name }) {
            Text(line).kithText(.body).foregroundStyle(Palette.ink)
        }
        Button(copied ? "Copied" : "Copy link") { UIPasteboard.general.string = invite.link; copied = true }.buttonStyle(.plain).foregroundStyle(Palette.ink)
        ShareLink(item: invite.link) { Label("Share link", systemImage: "square.and.arrow.up").foregroundStyle(Palette.ink) }
        Text("Single use, expires in 24 hours. Send it to them; they sign in with Google, Apple or GitHub.").kithText(.secondary).foregroundStyle(Palette.muted)
    }
}
