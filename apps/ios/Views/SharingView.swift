import KithKit
import KithUI
import SwiftUI

/// Settings > Sharing: one row that says who the owner shares with and opens the Sharing screen. Hidden on a host
/// that doesn't know invites.
struct SharingSection: View {
    let store: BotStore
    @State private var people: PeopleStore
    @State private var service: HostSharingService
    @State private var showing = false

    init(store: BotStore) {
        self.store = store
        #if DEBUG
        let people = DemoScreens.active ? PeopleStore.demo() : PeopleStore(store: store)
        #else
        let people = PeopleStore(store: store)
        #endif
        _people = State(initialValue: people)
        _service = State(initialValue: HostSharingService(people: people, agents: { agentsOffered(on: store) }))
    }

    var body: some View {
        Group {
            if !service.unsupported {
                CardSection("Sharing", footer: "Let a friend or teammate use some of your agents on \(store.hostName).") {
                    Button { showing = true } label: {
                        LinkRow {
                            HStack {
                                Label("Sharing", systemImage: "person.2")
                                Spacer(minLength: 8)
                                Text(summary).font(.subheadline).foregroundStyle(Palette.secondary).lineLimit(1)
                            }
                        }
                    }
                    .buttonStyle(.plain)
                }
            }
        }
        .task { await service.reload(); await store.botGuests.refresh() }
        .kithSheet(isPresented: $showing) { SharingView(service: service, people: people, store: store) }
    }

    private var summary: String {
        guard service.loaded else { return "" }
        let asking = store.accessRequests.count
        return asking == 0 ? service.summary : "\(service.summary) · \(asking) asking"
    }
}

/// Who the owner shares with, at a glance: what needs them first, then one row per person, then the Share button.
/// Plumbing (bots, activity, the old People screen) sits below, folded.
struct SharingView: View {
    let service: any SharingService
    let people: PeopleStore
    let store: BotStore
    @State private var sharing = false
    @State private var editing: SharingPerson?
    @State private var moreOpen = false
    @State private var showActivity = false
    @State private var showAdvanced = false
    @State private var botSheet: BotSheetTarget?

    var body: some View {
        CardForm {
            if let error = service.error {
                Text(error).font(.footnote).foregroundStyle(Palette.danger)
            }
            AccessRequestsSection(store: store, name: { service.person($0)?.name })
            if let gone = service.removed {
                HStack(spacing: 12) {
                    Text("\(gone.name) was removed.").font(.subheadline).foregroundStyle(Palette.secondary)
                    Spacer()
                    Button("Undo") { Task { await service.restore(person: gone.id) } }
                        .buttonStyle(.plain).foregroundStyle(Palette.accent).disabled(service.busy)
                    Button { service.dismissRemoved() } label: { Image(systemName: "xmark").foregroundStyle(Palette.tertiary) }
                        .buttonStyle(.plain).accessibilityLabel("Dismiss")
                }
            }
            if service.persons.isEmpty {
                if service.loaded { onboarding }
            } else {
                CardSection("People", footer: "Tap a person to change what they have.") {
                    ForEach(service.persons) { person in row(person) }
                }
                Button { sharing = true } label: { Text("Share").frame(maxWidth: .infinity, minHeight: 28) }
                    .buttonStyle(.primary)
            }
            more
            CardSection {
                Button { showAdvanced = true } label: {
                    LinkRow { Label("Advanced", systemImage: "slider.horizontal.3") }
                }
                .buttonStyle(.plain)
            }
        }
        .background(Palette.background)
        .safeAreaInset(edge: .top, spacing: 0) { ModalHeader("Sharing") }
        .refreshable { await service.reload(); await store.botGuests.refresh() }
        .task { await service.reload(); await store.botGuests.refresh() }
        .botSheets($botSheet, viewer: .owner)
        .kithSheet(isPresented: $sharing) { ShareFlowSheet(service: service, store: store) }
        .kithSheet(item: $editing) { person in SharingPersonSheet(personId: person.id, service: service, store: store) }
        .kithSheet(isPresented: $showActivity) { ActivitySheet(people: people) }
        .kithSheet(isPresented: $showAdvanced) { PeopleView(people: people, store: store) }
    }

    /// The empty state is also the owner's first look at what sharing does.
    private var onboarding: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("Let a friend or teammate chat with an agent, or let it work for them, on \(store.hostName). They sign in with their own account.")
                .foregroundStyle(Palette.secondary)
                .fixedSize(horizontal: false, vertical: true)
            Button { sharing = true } label: { Text("Share your first agent").frame(maxWidth: .infinity, minHeight: 28) }
                .buttonStyle(.primary)
        }
    }

    private func row(_ person: SharingPerson) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            if person.status == .removed {
                HStack(spacing: 12) {
                    header(person)
                    Spacer()
                    Button("Restore") { Task { await service.restore(person: person.id) } }
                        .buttonStyle(.plain).foregroundStyle(Palette.accent).disabled(service.busy)
                }
            } else {
                Button { editing = person } label: { LinkRow { header(person) } }.buttonStyle(.plain)
                if !person.shares.isEmpty {
                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 6) {
                            ForEach(person.shares) { share in
                                Text(share.chip)
                                    .font(.caption)
                                    .foregroundStyle(Palette.secondary)
                                    .padding(.horizontal, 8).padding(.vertical, 3)
                                    .background(Palette.line.opacity(0.5), in: Capsule())
                            }
                        }
                    }
                }
                if person.status == .waiting {
                    ForEach(person.openInvites, id: \.self) { invite in
                        HStack(spacing: 16) {
                            if let link = service.link(for: invite) {
                                ShareLink(item: link) { Label("Share link", systemImage: "square.and.arrow.up") }
                                Button("Copy") { UIPasteboard.general.string = link }.buttonStyle(.plain)
                            }
                            Button("Cancel", role: .destructive) { Task { await service.cancelInvite(invite) } }
                                .buttonStyle(.plain).foregroundStyle(Palette.danger).disabled(service.busy)
                        }
                        .font(.subheadline)
                        .foregroundStyle(Palette.accent)
                    }
                }
            }
        }
    }

    private func header(_ person: SharingPerson) -> some View {
        HStack(spacing: 12) {
            SkyPatch(id: person.id, size: 30)
            VStack(alignment: .leading, spacing: 2) {
                Text(person.name).font(.body.weight(.semibold))
                Text(person.status.title).font(.subheadline).foregroundStyle(Palette.secondary)
            }
        }
    }

    private var more: some View {
        VStack(alignment: .leading, spacing: 16) {
            Button { withAnimation { moreOpen.toggle() } } label: {
                HStack {
                    Text("More").font(.subheadline.weight(.semibold)).foregroundStyle(Palette.secondary)
                    Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(Palette.tertiary)
                        .rotationEffect(.degrees(moreOpen ? 90 : 0))
                    Spacer()
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            if moreOpen {
                YourBotsSection(store: store, botSheet: $botSheet)
                CardSection {
                    Button { showActivity = true } label: {
                        LinkRow { Label("Activity", systemImage: "list.bullet.rectangle") }
                    }
                    .buttonStyle(.plain)
                }
            }
        }
    }
}
