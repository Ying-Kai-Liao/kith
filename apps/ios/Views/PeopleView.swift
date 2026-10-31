import CoreImage.CIFilterBuiltins
import KithKit
import KithUI
import LocalAuthentication
import SwiftUI

/// The agents the owner has on a computer, which an invite can offer. Their own bots only: a chat an invited person has
/// is theirs and doesn't make a new agent.
@MainActor
func agentsOffered(on store: BotStore, plus extra: some Sequence<String> = [String]()) -> [String] {
    Array(Set(store.bots.values.compactMap(\.hqAgent).filter { !$0.isEmpty }).union(extra)).sorted()
}

/// Settings → People: who the owner invited to this computer. Hidden on a host that doesn't know invites.
struct PeopleSection: View {
    let store: BotStore
    @State private var people: PeopleStore
    @State private var showing = false

    init(store: BotStore) {
        self.store = store
        #if DEBUG
        _people = State(initialValue: DemoScreens.active ? PeopleStore.demo() : PeopleStore(store: store))
        #else
        _people = State(initialValue: PeopleStore(store: store))
        #endif
    }

    var body: some View {
        Group {
            if !people.unsupported {
                CardSection("People", footer: "Invite a friend or teammate to use some of your agents on \(store.hostName).") {
                    Button { showing = true } label: {
                        LinkRow {
                            Label(summary, systemImage: "person.2")
                        }
                    }
                    .buttonStyle(.plain)
                }
            }
        }
        .task { await people.load(); await store.botGuests.refresh() }
        .kithSheet(isPresented: $showing) { PeopleView(people: people, store: store) }
    }

    private var summary: String {
        guard people.loaded else { return "People" }
        let count = people.people.count
        let people = count == 0 ? "Invite someone" : count == 1 ? "1 person" : "\(count) people"
        let bots = store.botGuests.liveCount
        let summary = bots == 0 ? people : "\(count == 0 ? "0 people" : people) · \(bots == 1 ? "1 bot" : "\(bots) bots")"
        let asking = store.accessRequests.count
        return asking == 0 ? summary : "\(summary) · \(asking) waiting"
    }
}

/// The people, their agents and roles, what is waiting to be accepted, and the way to invite someone.
struct PeopleView: View {
    let people: PeopleStore
    let store: BotStore
    @State private var inviting = false
    @State private var editing: HostPerson?
    @State private var showActivity = false
    @State private var botSheet: BotSheetTarget?

    private var guests: BotGuestStore { store.botGuests }
    @State private var answering: Set<String> = []
    @State private var answerError: String?

    private func requester(_ request: MemberAccessRequest) -> String {
        request.personId.flatMap { people.person($0)?.name } ?? request.email ?? "Someone you invited"
    }

    /// Approving lets a new device in, so it asks for Face ID (or the passcode) first; Deny doesn't.
    private func answer(_ request: MemberAccessRequest, approve: Bool) {
        Task {
            answerError = nil
            if approve, !(await ownerConfirms("Let \(requester(request))'s \(request.deviceName) use \(store.hostName)")) { return }
            answering.insert(request.id)
            answerError = await store.decideAccessRequest(request, approve: approve)
            answering.remove(request.id)
        }
    }

    var body: some View {
        CardForm {
            if let error = people.error ?? answerError {
                Text(error).font(.footnote).foregroundStyle(Palette.danger)
            }
            if !store.accessRequests.isEmpty {
                CardSection("Asking to connect", footer: "Approve only if the code matches the one on their screen.") {
                    ForEach(store.accessRequests) { request in
                        AccessRequestRow(request: request, name: requester(request), busy: answering.contains(request.id),
                                         approve: { answer(request, approve: true) }, deny: { answer(request, approve: false) })
                    }
                }
            }
            CardSection("People", footer: people.people.isEmpty
                        ? "Nobody yet. They sign in to Kith with their own account, so you can see who did what."
                        : "Members and guests can only use the agents you pick. Tap a person to change that.") {
                if let gone = people.removed {
                    RemovedRow(name: gone.name, busy: people.busy,
                               undo: { Task { await people.restore(gone.personId) } }, dismiss: { people.dismissRemoved() })
                }
                ForEach(people.people) { person in
                    if person.revoked {
                        PersonRow(person: person, restore: { Task { await people.restore(person.personId) } }, busy: people.busy)
                    } else {
                        Button { editing = person } label: { PersonRow(person: person) }
                            .buttonStyle(.plain)
                    }
                    // That person's bots, as rows under them.
                    ForEach(botGuestRows(of: person, in: guests)) { guest in
                        BotGuestRow(bot: guest, viewer: .owner, store: store, indent: 20,
                                    open: { botSheet = BotSheetTarget(store: store, kind: .detail($0)) },
                                    newLink: { bot in Task { if let next = await BotSheetTarget.newLink(for: bot, on: store) { botSheet = next } } })
                    }
                }
                Button { inviting = true } label: {
                    Label("Invite a person", systemImage: "plus")
                        .foregroundStyle(Palette.accent)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            }

            if !guests.unsupported {
                CardSection("Your bots", footer: "Bots from Grok and other apps can ask the agents you pick. They never act as you.") {
                    ForEach(BotGuestStore.live(guests.mine)) { bot in
                        BotGuestRow(bot: bot, viewer: .owner, store: store,
                                    open: { botSheet = BotSheetTarget(store: store, kind: .detail($0)) },
                                    newLink: { bot in Task { if let next = await BotSheetTarget.newLink(for: bot, on: store) { botSheet = next } } })
                    }
                    Button { botSheet = .invite(on: store) } label: {
                        Label("Invite a bot", systemImage: "plus")
                            .foregroundStyle(Palette.accent)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    if !guests.mine.isEmpty { BotsCapRow(store: store) }
                    EndedBots(bots: BotGuestStore.ended(guests.mine), viewer: .owner, store: store)
                }
            }

            let pending = people.pending
            if !pending.isEmpty {
                CardSection("Waiting to join", footer: "Single use. Revoke a link to cancel it.") {
                    ForEach(pending) { invite in
                        PendingInviteRow(invite: invite, name: people.person(invite.personId)?.name ?? invite.personId) {
                            Task { await people.revokeInvite(invite.inviteId) }
                        }
                    }
                }
            }

            CardSection {
                Button { showActivity = true } label: {
                    LinkRow { Label("Activity", systemImage: "list.bullet.rectangle") }
                }
                .buttonStyle(.plain)
            }
        }
        .background(Palette.background)
        .safeAreaInset(edge: .top, spacing: 0) { ModalHeader("People") }
        .refreshable { await people.load(); await guests.refresh() }
        .task { await people.load(); await guests.refresh() }
        .botSheets($botSheet, viewer: .owner)
        .kithSheet(isPresented: $inviting) { InviteSheet(people: people, store: store) }
        .kithSheet(item: $editing) { person in EditPersonSheet(person: person, people: people, store: store) }
        .kithSheet(isPresented: $showActivity) { ActivitySheet(people: people) }
    }
}

/// The bots under one person: live ones, then ended ones (which the Your bots section folds).
@MainActor private func botGuestRows(of person: HostPerson, in guests: BotGuestStore) -> [BotGuest] {
    guests.bots(of: person.personId).filter { !$0.state.isEnded }
}

/// "Sam was removed. Undo", until the owner does something else.
private struct RemovedRow: View {
    let name: String
    let busy: Bool
    let undo: () -> Void
    let dismiss: () -> Void

    var body: some View {
        HStack(spacing: 12) {
            Text("\(name) was removed.").font(.subheadline).foregroundStyle(Palette.secondary)
            Spacer()
            Button("Undo", action: undo)
                .buttonStyle(.plain)
                .foregroundStyle(Palette.accent)
                .disabled(busy)
            Button(action: dismiss) { Image(systemName: "xmark").foregroundStyle(Palette.tertiary) }
                .buttonStyle(.plain)
                .accessibilityLabel("Dismiss")
        }
    }
}

private struct PersonRow: View {
    let person: HostPerson
    /// Set on a revoked person's row: its Restore access button.
    var restore: (() -> Void)?
    var busy = false

    var body: some View {
        if let restore {
            HStack(spacing: 12) {
                content
                Spacer()
                Button("Restore access", action: restore)
                    .buttonStyle(.plain)
                    .foregroundStyle(Palette.accent)
                    .disabled(busy)
            }
        } else {
            LinkRow { content }
        }
    }

    private var content: some View {
        HStack(spacing: 12) {
            SkyPatch(id: person.personId, size: 30)
            VStack(alignment: .leading, spacing: 2) {
                Text(person.name).font(.body.weight(.semibold))
                Text(roomsLine).font(.subheadline).foregroundStyle(Palette.secondary).lineLimit(1)
                Text(deviceLine).font(.caption).foregroundStyle(Palette.tertiary).lineLimit(1)
            }
        }
    }

    /// "teammate · Launch, Ship room": their role, then the rooms shared with them.
    private var roomsLine: String {
        let role = person.agents.values.contains(.member) ? "teammate" : "guest"
        return person.projects.isEmpty ? role : "\(role) · \(person.projects.joined(separator: ", "))"
    }

    private var deviceLine: String {
        if person.revoked { return "Access revoked" }
        guard person.joined else { return person.openInvites > 0 ? "Hasn't joined yet" : "Not joined" }
        let count = person.devices.count
        let devices = count == 1 ? "1 device" : "\(count) devices"
        if person.devices.contains(where: { $0.connected == true }) { return "\(devices) · online now" }
        return person.lastSeen.map { "\(devices) · last seen \(RelativeTime.day($0))" } ?? devices
    }
}

/// Face ID, Touch ID or the passcode; false when cancelled or this iPhone has none set up.
@MainActor
private func ownerConfirms(_ reason: String) async -> Bool {
    let context = LAContext()
    guard context.canEvaluatePolicy(.deviceOwnerAuthentication, error: nil) else { return false }
    return (try? await context.evaluatePolicy(.deviceOwnerAuthentication, localizedReason: reason)) ?? false
}

/// A member's new device: who, what it is, its code, and the answer buttons (Approve once it showed its code).
private struct AccessRequestRow: View {
    let request: MemberAccessRequest
    let name: String
    let busy: Bool
    let approve: () -> Void
    let deny: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            VStack(alignment: .leading, spacing: 2) {
                Text(name).font(.body.weight(.semibold)).foregroundStyle(Palette.text)
                Text(request.deviceName).font(.subheadline).foregroundStyle(Palette.secondary)
            }
            if let code = request.spacedCode {
                Text(code).font(.title2.monospacedDigit().weight(.semibold)).foregroundStyle(Palette.text)
                    .accessibilityLabel("Code \(code)")
            } else {
                Text("Waiting for their code…").font(.subheadline).foregroundStyle(Palette.secondary)
            }
            HStack(spacing: 16) {
                Button("Approve", action: approve)
                    .buttonStyle(.plain)
                    .foregroundStyle(Palette.accent)
                    .disabled(busy || request.code == nil)
                Button("Deny", action: deny)
                    .buttonStyle(.plain)
                    .foregroundStyle(Palette.danger)
                    .disabled(busy)
                if busy { ProgressView() }
            }
        }
    }
}

private struct PendingInviteRow: View {
    let invite: HostInvite
    let name: String
    let revoke: () -> Void

    var body: some View {
        HStack {
            VStack(alignment: .leading, spacing: 2) {
                Text(name).font(.body.weight(.semibold)).foregroundStyle(Palette.text)
                Text("Expires \(invite.expiry.formatted(.relative(presentation: .named)))")
                    .font(.subheadline).foregroundStyle(Palette.secondary)
            }
            Spacer()
            Button("Revoke", action: revoke)
                .buttonStyle(.plain)
                .foregroundStyle(Palette.danger)
        }
    }
}

// MARK: - invite

/// Pick a person (or type a new name), tick the agents and a role for each, then Create.
private struct InviteSheet: View {
    let people: PeopleStore
    let store: BotStore
    @State private var name = ""
    /// Nil is a new person.
    @State private var personId: String?
    @State private var roles: [String: InviteRole] = [:]
    @State private var created: CreatedInvite?

    private var agents: [String] { agentsOffered(on: store) }
    private var whoOptions: [(id: String, label: String)] {
        var options: [(id: String, label: String)] = [(id: "", label: "New person")]
        options += people.people.map { (id: $0.personId, label: $0.name) }
        return options
    }
    private var canCreate: Bool {
        !roles.isEmpty && !people.busy && (personId != nil || !name.trimmingCharacters(in: .whitespaces).isEmpty)
    }

    var body: some View {
        Group {
            if let created {
                InviteLinkView(invite: created, name: people.person(created.personId ?? "")?.name ?? name)
            } else {
                form
            }
        }
        .background(Palette.background)
        .safeAreaInset(edge: .top, spacing: 0) { ModalHeader("Invite someone") }
    }

    private var form: some View {
        CardForm {
            CardSection("Who", footer: "They sign in to Kith with their own Google, Apple or GitHub account.") {
                if !people.people.isEmpty {
                    ChoicePicker(selection: Binding(get: { personId ?? "" }, set: { personId = $0.isEmpty ? nil : $0 }),
                                 options: whoOptions)
                }
                if personId == nil {
                    TextField("Name", text: $name)
                        .textInputAutocapitalization(.words)
                        .submitLabel(.done)
                }
            }
            CardSection("Agents", footer: agents.isEmpty
                        ? "No agents on this computer yet."
                        : "A member can do more with an agent than a guest. Guest is the safer start.") {
                ForEach(agents, id: \.self) { agent in
                    AgentRoleRow(agent: agent, role: Binding(get: { roles[agent] }, set: { roles[agent] = $0 }))
                }
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
        }
    }

    private func create() async {
        let typed = name.trimmingCharacters(in: .whitespaces)
        if let made = await people.createInvite(name: personId == nil ? typed : nil, personId: personId, agents: roles) {
            created = made
        }
    }
}

/// An agent with a switch and, when it is on, the role.
private struct AgentRoleRow: View {
    let agent: String
    @Binding var role: InviteRole?

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Toggle(agent, isOn: Binding(get: { role != nil }, set: { role = $0 ? (role ?? .guest) : nil }))
                .toggleStyle(.kith)
            if let current = role {
                SegmentedChoice(selection: Binding(get: { current }, set: { role = $0 }),
                                options: InviteRole.allCases.map { ($0, $0.title) })
            }
        }
    }
}

/// The link, to copy or share, and a QR code; the words the owner passes on with it.
struct InviteLinkView: View {
    let invite: CreatedInvite
    let name: String
    @State private var copied = false

    var body: some View {
        CardForm {
            CardSection("Invite for \(name)", footer: "Single use, expires in 24 hours. Send it to them; they sign in with Google, Apple or GitHub.") {
                Text(invite.link)
                    .font(.system(.footnote, design: .monospaced))
                    .foregroundStyle(Palette.text)
                    .textSelection(.enabled)
                    .lineLimit(4)
                    .truncationMode(.middle)
                Button(copied ? "Copied" : "Copy link") {
                    UIPasteboard.general.string = invite.link
                    copied = true
                }
                .buttonStyle(.plain)
                .foregroundStyle(Palette.text)
                ShareLink(item: invite.link) {
                    Label("Share link", systemImage: "square.and.arrow.up")
                        .foregroundStyle(Palette.text)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
            if let code = QRCode.image(for: invite.link) {
                VStack(spacing: 8) {
                    Image(uiImage: code)
                        .interpolation(.none)
                        .resizable()
                        .scaledToFit()
                        .frame(width: 220, height: 220)
                        .padding(12)
                        .background(Color.white, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
                        .accessibilityLabel("QR code of the invite link")
                    Text("Or let them scan this with their camera.")
                        .font(.caption)
                        .foregroundStyle(Palette.secondary)
                }
                .frame(maxWidth: .infinity)
            }
        }
    }
}

private enum QRCode {
    static func image(for text: String) -> UIImage? {
        let filter = CIFilter.qrCodeGenerator()
        filter.message = Data(text.utf8)
        filter.correctionLevel = "M"
        guard let output = filter.outputImage else { return nil }
        let scaled = output.transformed(by: CGAffineTransform(scaleX: 8, y: 8))
        guard let cg = CIContext().createCGImage(scaled, from: scaled.extent) else { return nil }
        return UIImage(cgImage: cg)
    }
}

// MARK: - edit

/// Change a person's agents, roles and shared projects, or remove them.
private struct EditPersonSheet: View {
    let person: HostPerson
    let people: PeopleStore
    let store: BotStore
    @Environment(\.dismissModal) private var dismiss
    @State private var roles: [String: InviteRole]
    @State private var projects: Set<String>
    @State private var confirmRemove = false
    /// What the always-allow switch was just set to, until the people list says so.
    @State private var autoBots: Bool?

    init(person: HostPerson, people: PeopleStore, store: BotStore) {
        self.person = person
        self.people = people
        self.store = store
        _roles = State(initialValue: person.agents)
        _projects = State(initialValue: Set(person.projects))
    }

    private var agents: [String] { agentsOffered(on: store, plus: person.agents.keys) }
    private var changed: Bool { roles != person.agents || projects != Set(person.projects) }

    var body: some View {
        CardForm {
            CardSection("Agents") {
                ForEach(agents, id: \.self) { agent in
                    AgentRoleRow(agent: agent, role: Binding(get: { roles[agent] }, set: { roles[agent] = $0 }))
                }
            }
            if !people.projects.isEmpty || !projects.isEmpty {
                CardSection("Shared rooms", footer: "Only the rooms you tick are shared with \(person.name).") {
                    ForEach(projectChoices, id: \.id) { project in
                        Toggle(project.id, isOn: Binding(get: { projects.contains(project.id) },
                                                           set: { if $0 { projects.insert(project.id) } else { projects.remove(project.id) } }))
                            .toggleStyle(.kith)
                    }
                }
            }
            if !store.botGuests.unsupported {
                let theirs = store.botGuests.bots(of: person.personId)
                CardSection("\(person.name)'s bots", footer: "New bots \(person.name) invites start without waiting for you. Turning this on also allows any waiting now. Removing \(person.name) removes these too.") {
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
                        .disabled(store.botGuests.busy)
                    ForEach(BotGuestStore.live(theirs)) { bot in
                        BotGuestRow(bot: bot, viewer: .owner, store: store, indent: 0, open: { _ in }, newLink: { _ in })
                    }
                    if BotGuestStore.live(theirs).isEmpty {
                        Text("No bots yet").font(.subheadline).foregroundStyle(Palette.secondary)
                    }
                }
            }
            if let error = people.error {
                Text(error).font(.footnote).foregroundStyle(Palette.danger)
            }
            Button {
                Task {
                    if await people.setAccess(person.personId, agents: roles, projects: projects.sorted()) { dismiss() }
                }
            } label: {
                Text("Save").frame(maxWidth: .infinity, minHeight: 28)
            }
            .buttonStyle(.primary)
            .disabled(!changed || people.busy)

            CardSection {
                Button { confirmRemove = true } label: {
                    Label("Remove \(person.name)", systemImage: "person.badge.minus")
                        .foregroundStyle(Palette.danger)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .disabled(people.busy)
            }
        }
        .background(Palette.background)
        .safeAreaInset(edge: .top, spacing: 0) { ModalHeader(person.name) }
        .task { await people.loadProjects() }
        .kithDialog("Remove \(person.name)?", isPresented: $confirmRemove,
                    message: "They lose access to this computer. Their memory stays on the Mac.") {
            [DialogAction("Remove", destructive: true) {
                Task { if await people.remove(person.personId) { dismiss() } }
            }]
        }
    }

    /// Projects the host listed, plus any already shared that it no longer lists.
    private var projectChoices: [HQProject] {
        let known = Set(people.projects.map(\.id))
        return people.projects + projects.subtracting(known).sorted().map { HQProject(id: $0) }
    }
}

// MARK: - activity

private struct ActivitySheet: View {
    let people: PeopleStore

    var body: some View {
        CardForm {
            CardSection("Activity", footer: people.audit.isEmpty ? "Nothing yet. What people ask this computer to do shows here for 90 days." : nil) {
                ForEach(people.audit) { row in
                    HStack(alignment: .firstTextBaseline) {
                        VStack(alignment: .leading, spacing: 2) {
                            Text("\(name(row.personId)) · \(row.method)").foregroundStyle(Palette.text)
                            Text(row.time.formatted(date: .abbreviated, time: .shortened))
                                .font(.caption).foregroundStyle(Palette.tertiary)
                        }
                        Spacer()
                        Text(row.allowed ? "Allowed" : "Denied")
                            .font(.subheadline)
                            .foregroundStyle(row.allowed ? Palette.secondary : Palette.danger)
                    }
                }
            }
            if let error = people.error {
                Text(error).font(.footnote).foregroundStyle(Palette.danger)
            }
        }
        .background(Palette.background)
        .safeAreaInset(edge: .top, spacing: 0) { ModalHeader("Activity") }
        .refreshable { await people.loadAudit() }
        .task { await people.loadAudit() }
    }

    private func name(_ id: String) -> String { people.person(id)?.name ?? id }
}

/// Settings → Your bots: for an invited person, the bots they invited to the computers they are members of. Shown
/// only where the computer answers `myBots`.
struct BotsSection: View {
    let stores: [BotStore]
    @State private var showing = false

    private var shown: [BotStore] { stores.filter { !$0.botGuests.unsupported && $0.botGuests.loaded } }

    var body: some View {
        Group {
            if !shown.isEmpty {
                let count = shown.reduce(0) { $0 + BotGuestStore.live($1.botGuests.bots).count }
                CardSection("Your bots", footer: "Bots from Grok and other apps can ask the agents you were given. They never act as you.") {
                    Button { showing = true } label: {
                        LinkRow {
                            Label(count == 0 ? "Invite a bot" : count == 1 ? "1 bot" : "\(count) bots",
                                  systemImage: "bubble.left.and.bubble.right")
                        }
                    }
                    .buttonStyle(.plain)
                }
            }
        }
        .kithSheet(isPresented: $showing) { BotsView(stores: shown) }
    }
}

#if DEBUG
/// The `-kithDemoScreens bots-*` shots: each People and bot sheet over the demo account.
struct DemoBotScreen: View {
    let store: BotStore
    let screen: String

    private var sam: HostPerson { HostPerson(personId: "sam", name: "Sam", agents: ["ada": .member, "remy": .guest], joined: true) }

    var body: some View {
        let people = PeopleStore.demo()
        switch screen {
        case "bots-people": PeopleView(people: people, store: store).task { await people.load() }
        case "bots-edit": EditPersonSheet(person: sam, people: people, store: store)
        case "bots-invite": InviteBotSheet(store: store)
        case "bots-result": InviteBotSheet(store: store, result: InviteBotSheet.Result(
            invite: CreatedBotInvite(botId: "bg", inviteId: "bi", expiresAt: Int64((Date.now.timeIntervalSince1970 + 3_600) * 1000),
                                     link: "https://kith.example/b/demo", kithLink: nil,
                                     share: "Install the Kith client, then join my Kith computer. This link works once and expires in 1 hour.\n\n  curl -fsSL https://example.invalid/install-guest.sh | sh\n  kith join 'https://kith.example/b/demo#t=demo'\n\nAfterwards ask my agents with:  kith ask @ada \"your question\"\nRun `kith --help` for the rest.\n"),
            name: "Rex", provider: "grok"))
        case "bots-detail", "bots-remove", "botguest-detail":
            if let bot = store.botGuests.bot("bg_act") { BotDetailView(bot: bot, viewer: .owner, store: store, askRemove: screen == "bots-remove") }
        default: EmptyView()
        }
    }
}
#endif
