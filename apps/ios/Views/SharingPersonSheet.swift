import KithKit
import KithUI
import SwiftUI

/// One person: what is shared with them, each with its level and a way to remove it, adding more, and stopping it all.
struct SharingPersonSheet: View {
    let personId: String
    let service: any SharingService
    let store: BotStore
    @Environment(\.dismissModal) private var dismiss
    @State private var adding = false
    @State private var confirmStop = false
    @State private var botsOpen = false
    /// What the always-allow switch was just set to, until the list says so.
    @State private var autoBots: Bool?

    private var person: SharingPerson? { service.person(personId) }

    var body: some View {
        Group {
            if let person {
                CardForm {
                    if let error = service.error {
                        Text(error).font(.footnote).foregroundStyle(Palette.danger)
                    }
                    if person.status == .removed {
                        CardSection(footer: "\(person.name) lost access to this computer. Their chats and memory stay on it.") {
                            Button("Restore access") { Task { await service.restore(person: person.id) } }
                                .buttonStyle(.plain).foregroundStyle(Palette.accent).disabled(service.busy)
                        }
                    } else {
                        shares(of: person)
                        if !person.openInvites.isEmpty {
                            CardSection("Waiting to join", footer: "Single use. Cancel a link to take it back.") {
                                ForEach(person.openInvites, id: \.self) { invite in
                                    HStack {
                                        Text("Invite link").foregroundStyle(Palette.text)
                                        Spacer()
                                        Button("Cancel") { Task { await service.cancelInvite(invite) } }
                                            .buttonStyle(.plain).foregroundStyle(Palette.danger).disabled(service.busy)
                                    }
                                }
                            }
                        }
                        bots(of: person)
                        CardSection {
                            Button { confirmStop = true } label: {
                                Label("Stop sharing everything", systemImage: "person.badge.minus")
                                    .foregroundStyle(Palette.danger)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                                    .contentShape(Rectangle())
                            }
                            .buttonStyle(.plain)
                            .disabled(service.busy)
                        }
                    }
                }
            } else {
                Color.clear
            }
        }
        .background(Palette.background)
        .safeAreaInset(edge: .top, spacing: 0) { ModalHeader(person?.name ?? "") }
        .task { await store.refreshAllRoomPeople() }
        .kithSheet(isPresented: $adding) { ShareFlowSheet(service: service, store: store, person: personId) }
        .kithDialog("Stop sharing with \(person?.name ?? "")?", isPresented: $confirmStop,
                    message: ["They lose access to this computer. Their memory stays on the Mac.",
                              RoomPeopleLogic.revokeLine(person: person?.name ?? "", rooms: store.rooms(of: personId).map(\.name))]
                        .compactMap { $0 }.joined(separator: " ")) {
            [DialogAction("Stop sharing", destructive: true) {
                // The Sharing screen offers Undo while the person stays in `removed`.
                Task { if await service.stopSharing(person: personId) { dismiss() } }
            }]
        }
    }

    private func shares(of person: SharingPerson) -> some View {
        CardSection("Shared", footer: person.shares.isEmpty ? "Nothing yet." : nil) {
            ForEach(person.shares) { share in row(share) }
            Button { adding = true } label: {
                Label("Share something", systemImage: "plus")
                    .foregroundStyle(Palette.accent)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .disabled(service.busy)
        }
    }

    private func change(_ level: ShareLevel, of agent: String) {
        Task { _ = await service.setLevel(level, of: agent, for: personId) }
    }

    @ViewBuilder private func row(_ share: Share) -> some View {
        if share.everyone {
            // Everyone's agents are the owner's to change for everyone, not per person.
            HStack {
                Text(share.what.name).foregroundStyle(Palette.text)
                Spacer()
                Text("Everyone · \(share.level?.phrase ?? "")").font(.subheadline).foregroundStyle(Palette.secondary)
            }
        } else {
            VStack(alignment: .leading, spacing: 10) {
                HStack {
                    Text(share.what.name).font(.body.weight(.semibold)).foregroundStyle(Palette.text)
                    if !share.what.isAgent { Text("Room").font(.subheadline).foregroundStyle(Palette.secondary) }
                    Spacer()
                    Button("Remove") { Task { _ = await service.unshare(share.what, from: personId) } }
                        .buttonStyle(.plain).foregroundStyle(Palette.danger).disabled(service.busy)
                }
                if case .agent(let agent) = share.what, let level = share.level {
                    SegmentedChoice(selection: Binding(get: { level }, set: { change($0, of: agent) }), options: ShareLevel.allCases.map { ($0, $0.title) })
                    .disabled(service.busy)
                }
            }
        }
    }

    /// Their bots, folded away: bots are plumbing for most owners.
    @ViewBuilder private func bots(of person: SharingPerson) -> some View {
        if !store.botGuests.unsupported {
            let theirs = BotGuestStore.live(store.botGuests.bots(of: person.id))
            VStack(alignment: .leading, spacing: 16) {
                Button { withAnimation { botsOpen.toggle() } } label: {
                    HStack {
                        Text("\(person.name)'s bots").font(.subheadline.weight(.semibold)).foregroundStyle(Palette.secondary)
                        Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(Palette.tertiary)
                            .rotationEffect(.degrees(botsOpen ? 90 : 0))
                        Spacer()
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                if botsOpen {
                    CardSection(footer: "New bots \(person.name) invites start without waiting for you. Turning this on also allows any waiting now. Stopping sharing removes these too.") {
                        Toggle("Always allow \(person.name)'s bots", isOn: Binding(
                            get: { autoBots ?? person.autoBots },
                            set: { on in
                                autoBots = on
                                Task {
                                    let done = await store.botGuests.setAutoAllow(personId: person.id, on: on)
                                    await service.reload()
                                    if !done { autoBots = nil }
                                }
                            }))
                            .toggleStyle(.kith)
                            .disabled(store.botGuests.busy)
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
    }
}
