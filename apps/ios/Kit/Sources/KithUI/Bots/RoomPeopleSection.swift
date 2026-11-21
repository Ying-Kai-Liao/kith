import KithKit
import SwiftUI

/// What the Add sheet opens on: a person to give agents to (a needs-you row) or add back.
struct PersonPreset: Identifiable, Hashable {
    var personId: String?
    /// Give the missing agents only; the person is in the room already.
    var giveOnly = false
    /// Open "Invite someone" with the person picked (their invite ran out).
    var invite = false
    var id: String { "\(personId ?? "new")-\(giveOnly)-\(invite)" }
}

/// Room info > People (SPEC §1): needs-you rows first, then the owner, people in the room, invited, and a collapsed
/// "Earlier". Hidden while the host's `rooms.people` setting is off: then only the owner's own row shows.
struct RoomPeopleSection: View {
    @Environment(BotStore.self) private var model
    let group: Bot
    let store: RoomPeopleStore
    let openAdd: (PersonPreset?) -> Void
    @State private var removing: HostPerson?
    @State private var cancelling: InvitedRow?
    @State private var showEarlier = false

    private var parts: (agents: [RoomAgent], outside: [String]) {
        RoomPeopleLogic.parts(members: group.members, find: { model.bots[$0] }, outsideName: { _ in "Bot" })
    }

    // Off hides the section, unless someone still has an open stay: the owner can still remove them.
    private var enabled: Bool { (store.enabled == true || store.stays.contains { $0.leftGen == nil }) && !store.people.unsupported }

    var body: some View {
        let agents = parts.agents
        let people = store.people.people
        let needs = RoomPeopleLogic.needsKai(stays: store.stays, people: people, agents: agents)
        let members = RoomPeopleLogic.inTheRoom(stays: store.stays, people: people, agents: agents)
        let invited = RoomPeopleLogic.invitedRows(invites: store.people.invites, people: people, room: group.id, agents: agents)
        let gone = RoomPeopleLogic.earlier(stays: store.stays, people: people).filter { p in !invited.contains { $0.invite.personId == p.personId } }
        VStack(alignment: .leading, spacing: 8) {
            head
            ForEach(needs) { needsRow($0, people: people) }
            VStack(spacing: 0) {
                row(face: Circle().fill(LinearGradient(colors: [Color(hex: 0xF9C3D3), Color(hex: 0xF4A6C4)], startPoint: .top, endPoint: .bottom)), title: "You", detail: "Owner", trailing: EmptyView())
                if enabled {
                    ForEach(members) { person in
                        Divider().padding(.leading, 64)
                        row(face: Circle().fill(Palette.surface), title: person.name, detail: seen(person), trailing: model.isOwner ? AnyView(action("Remove", color: Palette.danger) { removing = person }) : AnyView(EmptyView()))
                    }
                    ForEach(invited) { item in
                        Divider().padding(.leading, 64)
                        row(face: Image(systemName: "person.badge.plus").foregroundStyle(Palette.muted), title: item.name, detail: invitedLine(item), trailing: invitedAction(item))
                    }
                    if !gone.isEmpty {
                        Divider().padding(.leading, 64)
                        Button { withAnimation(Motion.layout) { showEarlier.toggle() } } label: {
                            HStack { Text("Earlier · \(gone.count) \(gone.count == 1 ? "person" : "people")").kithText(.secondary).foregroundStyle(Palette.muted); Spacer(); Image(systemName: showEarlier ? "chevron.down" : "chevron.right").font(.caption).foregroundStyle(Palette.muted) }
                                .padding(.horizontal, 14).frame(minHeight: 44).contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel("Earlier, \(gone.count) \(gone.count == 1 ? "person" : "people")")
                        if showEarlier {
                            ForEach(gone) { person in
                                row(face: Circle().fill(Palette.surface), title: person.name, detail: "", trailing: AnyView(action("Add back", color: Palette.accent) { openAdd(PersonPreset(personId: person.personId)) }))
                            }
                        }
                    }
                }
            }
            .background(Palette.background, in: RoundedRectangle(cornerRadius: KithRadius.card, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: KithRadius.card, style: .continuous).strokeBorder(Palette.border, lineWidth: 1))
            if enabled, !(members.isEmpty && needs.isEmpty && invited.isEmpty) {
                Text("People see what's said from when they join. Agents answer them only when @'d. Every card comes to you.")
                    .kithText(.secondary).foregroundStyle(Palette.muted).padding(.horizontal, 4)
            }
            if let error = store.error { Text(error).kithText(.secondary).foregroundStyle(Palette.danger).padding(.horizontal, 4) }
        }
        .kithDialog("Remove \(removing?.name ?? "this person") from \(group.name)?", isPresented: Binding(get: { removing != nil }, set: { if !$0 { removing = nil } }),
                    message: "\(removing?.name ?? "They") stops seeing \(group.name) now. If you add \(removing?.name ?? "them") again, they see only what's said after that.") {
            let target = removing
            return [DialogAction("Remove", destructive: true) { if let target { Task { _ = await store.remove(target.personId) } } }]
        }
        .kithDialog("Cancel \(cancelling?.name ?? "the")'s invite?", isPresented: Binding(get: { cancelling != nil }, set: { if !$0 { cancelling = nil } }),
                    message: "The link stops working. \(cancelling?.name ?? "They") won't join \(group.name).", cancel: "Keep") {
            let target = cancelling
            return [DialogAction("Cancel invite", destructive: true) { if let target { Task { await store.people.revokeInvite(target.invite.inviteId); await store.load() } } }]
        }
    }

    private var head: some View {
        HStack {
            Text("PEOPLE").font(.system(size: 13, weight: .semibold)).tracking(0.8).foregroundStyle(Palette.muted)
            Spacer()
            if enabled, model.isOwner {
                Button { openAdd(nil) } label: {
                    Text("Add").font(.system(size: 16, weight: .semibold)).foregroundStyle(Palette.accent).frame(minHeight: 44).contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            }
        }
        .padding(.leading, 4)
    }

    private func needsRow(_ r: NeedsKaiRow, people: [HostPerson]) -> some View {
        let person = people.first { $0.personId == r.personId }
        return VStack(alignment: .leading, spacing: 6) {
            Text("\(r.name) can't open \(group.name)").kithText(.topBar).foregroundStyle(Palette.ink)
            Text(r.kind == .revoked ? "\(r.name)'s access is revoked. No agent answers here until \(r.name) is removed or restored." : RoomPeopleLogic.lackLine(r.missing))
                .kithText(.secondary).foregroundStyle(Palette.muted).fixedSize(horizontal: false, vertical: true)
            HStack(spacing: 8) {
                if r.kind == .missing {
                    pill("Give \(RoomPeopleLogic.joined(r.missing.map(\.name)))", ink: true) { openAdd(PersonPreset(personId: r.personId, giveOnly: true)) }
                    pill("Remove", ink: false) { removing = person }
                } else {
                    pill("Remove", ink: true) { removing = person }
                }
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Palette.surface, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
        .accessibilityElement(children: .contain)
    }

    private func pill(_ title: String, ink: Bool, _ run: @escaping () -> Void) -> some View {
        Button(action: run) {
            Text(title).font(.system(size: 15, weight: .semibold)).foregroundStyle(ink ? Palette.background : Palette.ink)
                .padding(.horizontal, 14).frame(minHeight: 44)
                .background(ink ? AnyShapeStyle(Palette.ink) : AnyShapeStyle(Color.clear), in: Capsule())
                .overlay(Capsule().strokeBorder(Palette.border, lineWidth: ink ? 0 : 1))
        }
        .buttonStyle(.plain).disabled(store.busy)
    }

    private func action(_ title: String, color: Color, _ run: @escaping () -> Void) -> some View {
        Button(action: run) { Text(title).font(.system(size: 16, weight: .semibold)).foregroundStyle(color).frame(minWidth: 60, minHeight: 44).contentShape(Rectangle()) }
            .buttonStyle(.plain).disabled(store.busy)
    }

    private func invitedLine(_ item: InvitedRow) -> String {
        switch item.state {
        case .ranOut: "\(item.name)'s invite ran out"
        case .waiting: "Invited · joins when \(item.name) accepts"
        case .accepted: item.missing.isEmpty ? "Accepted · joining…" : "Accepted · joins once \(item.name) has \(RoomPeopleLogic.joined(item.missing.map(\.name)))"
        }
    }

    private func invitedAction(_ item: InvitedRow) -> AnyView {
        switch item.state {
        case .ranOut: AnyView(action("Invite again", color: Palette.accent) { openAdd(PersonPreset(personId: item.invite.personId, invite: true)) })
        case .accepted where !item.missing.isEmpty: AnyView(action("Give \(RoomPeopleLogic.joined(item.missing.map(\.name)))", color: Palette.accent) { openAdd(PersonPreset(personId: item.invite.personId, giveOnly: true)) })
        case .waiting: AnyView(action("Cancel", color: Palette.danger) { cancelling = item })
        default: AnyView(EmptyView())
        }
    }

    private func seen(_ person: HostPerson) -> String {
        if person.devices.contains(where: { $0.connected == true }) { return "Active now" }
        guard let last = person.lastSeen else { return "" }
        let hours = max(1, Int(Date.now.timeIntervalSince(last) / 3600))
        return hours >= 48 ? "Seen \(hours / 24) d ago" : "Seen \(hours) h ago"
    }

    private func row(face: some View, title: String, detail: String, trailing: some View) -> some View {
        HStack(spacing: 14) {
            face.frame(width: 40, height: 40)
            VStack(alignment: .leading, spacing: 2) {
                Text(title).kithText(.topBar).foregroundStyle(Palette.ink).lineLimit(1)
                if !detail.isEmpty { Text(detail).kithText(.secondary).foregroundStyle(Palette.muted).fixedSize(horizontal: false, vertical: true) }
            }
            Spacer(minLength: 8)
            trailing
        }
        .padding(.horizontal, 14).padding(.vertical, 10).frame(minHeight: 56)
    }
}

/// Room info > People > Add (SPEC §2): invited people split into "Can join" and "Needs access", the join card and the
/// grant card (give access and add in one step). A room with an outside bot takes no people; the sheet says so.
struct AddPersonSheet: View {
    @Environment(BotStore.self) private var model
    @Environment(\.dismissModal) private var dismiss
    let group: Bot
    let store: RoomPeopleStore
    let preset: PersonPreset?
    @State private var pick: String?
    @State private var roles: [String: InviteRole] = [:]
    @State private var inviting: String?
    @State private var invitingNew = false

    init(group: Bot, store: RoomPeopleStore, preset: PersonPreset?) {
        self.group = group
        self.store = store
        self.preset = preset
        _pick = State(initialValue: preset?.invite == true ? nil : preset?.personId)
        _invitingAgain = State(initialValue: preset?.invite == true ? preset?.personId : nil)
    }
    @State private var invitingAgain: String?

    private var parts: (agents: [RoomAgent], outside: [String]) {
        RoomPeopleLogic.parts(members: group.members, find: { model.bots[$0] }, outsideName: { _ in "Bot" })
    }

    private var giveOnly: Bool { preset?.giveOnly == true && pick == preset?.personId }

    var body: some View {
        let agents = parts.agents
        let block = RoomPeopleLogic.block(agents: agents, outside: parts.outside)
        let groups = RoomPeopleLogic.pickerGroups(people: store.people.people, stays: store.stays, agents: agents)
        let picked = store.people.people.first { $0.personId == pick }
        VStack(spacing: 0) {
            ModalHeader("Add to \(group.name)") {}
            ScrollView {
                VStack(alignment: .leading, spacing: 22) {
                    if let block {
                        Text(block.text(room: group.name)).kithText(.body).foregroundStyle(Palette.muted)
                    } else {
                        if groups.canJoin.isEmpty && groups.needsAccess.isEmpty { Text("You haven't invited anyone yet.").kithText(.body).foregroundStyle(Palette.muted) }
                        if !groups.canJoin.isEmpty { list("Can join") { ForEach(groups.canJoin) { choice($0, agents: agents) } } }
                        if !groups.needsAccess.isEmpty { list("Needs access") { ForEach(groups.needsAccess) { choice($0, agents: agents) } } }
                    }
                    if block == nil {
                        Button { invitingNew = true } label: {
                            HStack(spacing: 14) {
                                Image(systemName: "person.badge.plus").foregroundStyle(Palette.muted).frame(width: 40, height: 40)
                                Text("Invite someone new…").kithText(.topBar).foregroundStyle(Palette.ink)
                                Spacer()
                            }
                            .padding(.horizontal, 14).frame(minHeight: 56).contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .overlay(RoundedRectangle(cornerRadius: KithRadius.card, style: .continuous).strokeBorder(Palette.border, lineWidth: 1))
                    }
                    if let picked, block == nil { card(picked, agents: agents) }
                    if let error = store.error { Text(error).kithText(.secondary).foregroundStyle(Palette.danger) }
                }
                .padding(.horizontal, 20).padding(.bottom, 24)
            }
        }
        .background(Palette.background)
        .animation(Motion.layout, value: pick)
        .task { await store.load(); await model.refreshAllRoomPeople() }
        .onChange(of: pick) { roles = [:] }
        .kithSheet(item: Binding(get: { invitingAgain.map { IdBox(id: $0) } }, set: { invitingAgain = $0?.id })) { box in
            RoomInviteSheet(group: group, agents: parts.agents, people: store.people, personId: box.id)
        }
        .kithSheet(isPresented: $invitingNew) { RoomInviteSheet(group: group, agents: parts.agents, people: store.people, personId: nil) }
    }

    private func list(_ title: String, @ViewBuilder content: () -> some View) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title.uppercased()).font(.system(size: 13, weight: .semibold)).tracking(0.8).foregroundStyle(Palette.muted).padding(.leading, 4)
            VStack(spacing: 0) { content() }.overlay(RoundedRectangle(cornerRadius: KithRadius.card, style: .continuous).strokeBorder(Palette.border, lineWidth: 1))
        }
    }

    private func choice(_ r: PickerRow, agents: [RoomAgent]) -> some View {
        let detail = !r.missing.isEmpty ? "Doesn't have \(RoomPeopleLogic.joined(r.missing.map(\.name)))"
            : !r.accepted ? "Hasn't accepted the invite yet · sees \(group.name) once \(r.person.name) does" : "Has \(RoomPeopleLogic.joined(agents.map(\.name)))"
        return Button { pick = pick == r.id ? nil : r.id } label: {
            HStack(spacing: 14) {
                Circle().fill(Palette.surface).frame(width: 40, height: 40)
                VStack(alignment: .leading, spacing: 2) {
                    Text(r.person.name).kithText(.topBar).foregroundStyle(Palette.ink).lineLimit(1)
                    Text(detail).kithText(.secondary).foregroundStyle(Palette.muted).fixedSize(horizontal: false, vertical: true)
                }
                Spacer(minLength: 8)
                if pick == r.id { Image(systemName: "checkmark.circle.fill").foregroundStyle(Palette.ink) }
            }
            .padding(.horizontal, 14).padding(.vertical, 10).frame(minHeight: 56).contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(pick == r.id ? .isSelected : [])
    }

    @ViewBuilder private func card(_ person: HostPerson, agents: [RoomAgent]) -> some View {
        let missing = RoomPeopleLogic.missing(person, agents: agents)
        let names = RoomPeopleLogic.joined(missing.map(\.name))
        VStack(alignment: .leading, spacing: 12) {
            if missing.isEmpty {
                Text("\(person.name) joins \(group.name).").kithText(.topBar).foregroundStyle(Palette.ink)
                fact("eye", "\(person.name) sees what's said from now on, not before.")
                fact("at", "\(RoomPeopleLogic.joined(agents.map(\.name))) answer \(person.name) only when \(person.name) @'s them. Every card comes to you.")
            } else {
                Text(giveOnly ? "Give \(person.name) \(names)." : "Give \(person.name) \(names) to add \(person.name).").kithText(.topBar).foregroundStyle(Palette.ink)
                ForEach(missing) { agent in
                    HStack {
                        Text(agent.name).kithText(.agentName).foregroundStyle(Palette.ink)
                        Spacer()
                        Picker(agent.name, selection: Binding(get: { roles[agent.id] ?? .guest }, set: { roles[agent.id] = $0 })) {
                            ForEach(InviteRole.allCases, id: \.self) { Text($0.title).tag($0) }
                        }
                        .pickerStyle(.segmented).frame(maxWidth: 170)
                    }
                }
                Text("\(person.name) can then use \(names) in a 1:1 too.").kithText(.secondary).foregroundStyle(Palette.muted)
            }
            HStack(spacing: 10) {
                Button { Task { await finish(person, missing: missing) } } label: {
                    Group { if store.busy { Spinner(size: 14) } else { Text(missing.isEmpty ? "Add \(person.name)" : giveOnly ? "Give access" : "Give access and add").font(.system(size: 17, weight: .semibold)) } }
                        .foregroundStyle(Palette.background).frame(maxWidth: .infinity, minHeight: 48).background(Palette.ink, in: Capsule())
                }
                .buttonStyle(.plain).disabled(store.busy)
                Button { pick = nil } label: {
                    Text("Cancel").font(.system(size: 17, weight: .semibold)).foregroundStyle(Palette.ink)
                        .frame(maxWidth: .infinity, minHeight: 48).overlay(Capsule().strokeBorder(Palette.border, lineWidth: 1))
                }
                .buttonStyle(.plain).disabled(store.busy)
            }
        }
        .padding(16).frame(maxWidth: .infinity, alignment: .leading)
        .background(Palette.surface, in: RoundedRectangle(cornerRadius: KithRadius.card, style: .continuous))
    }

    private func fact(_ icon: String, _ text: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Image(systemName: icon).font(.system(size: 13)).foregroundStyle(Palette.muted).frame(width: 18)
            Text(text).kithText(.body).foregroundStyle(Palette.ink).fixedSize(horizontal: false, vertical: true)
        }
    }

    private func finish(_ person: HostPerson, missing: [RoomAgent]) async {
        var given: [String: InviteRole] = [:]
        for agent in missing { given[agent.id] = roles[agent.id] ?? .guest }
        let names = missing.map(\.name)
        let ok = missing.isEmpty ? await store.add(person.personId) : giveOnly ? await store.give(person, given) : await store.grantAndAdd(person, given, names: names)
        if ok { dismiss() }
    }
}

private struct IdBox: Identifiable { var id: String }
