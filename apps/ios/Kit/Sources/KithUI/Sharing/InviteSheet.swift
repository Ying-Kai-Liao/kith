import CoreImage.CIFilterBuiltins
import KithKit
import SwiftUI
import UIKit

/// "Add a friend": who, which agents each at Can ask or Can work, and optional group rooms. Makes a one-time invite
/// (24 hours, for this computer) and shows its link and QR code. `prefill` keeps the caller's context: an agent, a
/// room, someone already invited, or nothing.
public struct InviteSheet: View {
    let store: BotStore
    @State private var model: InviteSheetModel
    private let lockedPerson: Bool

    /// `people` is the caller's list when it has one; without it the sheet reads its own.
    public init(store: BotStore, people: PeopleStore? = nil, prefill: InvitePrefill = InvitePrefill(), created: CreatedInvite? = nil) {
        self.store = store
        let people = people ?? PeopleStore(store: store)
        lockedPerson = prefill.personId != nil
        _model = State(initialValue: InviteSheetModel(people: people, offered: Self.offered(on: store, plus: prefill.agents.keys),
                                                      rooms: Self.rooms(on: store), prefill: prefill, online: store.client != nil, created: created))
    }

    /// The agents on the computer an invite can offer: the owner's own hq agents, never an invited person's chat.
    static func offered(on store: BotStore, plus extra: some Sequence<String>) -> [RoomAgent] {
        var names: [String: String] = [:]
        for bot in store.bots.values.sorted(by: { $0.id < $1.id }) { if let id = bot.hqAgent, !id.isEmpty { names[id] = bot.name } }
        for id in extra where names[id] == nil { names[id] = id }
        return names.map { RoomAgent(id: $0.key, name: $0.value) }.sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
    }

    static func rooms(on store: BotStore) -> [InviteSheetModel.Room] {
        store.inviteRooms.map { InviteSheetModel.Room(id: $0.id, name: $0.name, agents: $0.agents) }
    }

    public var body: some View {
        Group {
            if let done = model.updated {
                CardForm {
                    CardSection {
                        Label("Shared with \(model.people.person(done)?.name ?? model.who).", systemImage: "checkmark.circle")
                            .foregroundStyle(Palette.text)
                    }
                }
            } else if let made = model.created {
                InviteLinkView(invite: made, name: model.people.person(made.personId ?? "")?.name ?? model.who, joins: model.joinsLine)
            } else {
                form
            }
        }
        .background(Palette.background)
        .safeAreaInset(edge: .top, spacing: 0) { ModalHeader(model.updated != nil ? "Shared" : model.created == nil ? "Add a friend" : "Invite ready") }
        .task {
            await model.people.load()
            await model.people.loadProjects()
            await store.refreshAllRoomPeople()
            model.rooms = Self.rooms(on: store)
        }
        .onChange(of: store.client == nil) { _, offline in model.online = !offline }
    }

    private var whoOptions: [(id: String, label: String)] {
        [(id: "", label: "New person")] + model.people.people.filter { !$0.revoked }.map { (id: $0.personId, label: $0.name) }
    }

    private var form: some View {
        CardForm {
            CardSection("To", footer: "They sign in to Kith with their own Google, Apple or GitHub account.") {
                if lockedPerson {
                    Text(model.who).foregroundStyle(Palette.text)
                } else {
                    if !model.people.people.isEmpty {
                        ChoicePicker(selection: Binding(get: { model.personId ?? "" }, set: { model.personId = $0.isEmpty ? nil : $0 }), options: whoOptions)
                    }
                    if model.personId == nil {
                        TextField("Name", text: $model.name)
                            .textInputAutocapitalization(.words)
                            .submitLabel(.done)
                    }
                }
            }
            CardSection("Share agents", footer: model.offered.isEmpty ? "No agents on this computer yet."
                        : "Can ask: answers, asks you before changing anything. Can work: can make changes.") {
                ForEach(model.offered) { agent in agentRow(agent) }
            }
            if !model.rooms.isEmpty {
                CardSection("Group rooms") {
                    ForEach(model.rooms, id: \.id) { room in
                        let needs = RoomPeopleLogic.roomNeeds(room.agents, roles: model.roles)
                        Toggle(needs.isEmpty ? room.name : "\(room.name) · Needs \(RoomPeopleLogic.joined(needs.map(\.name)))",
                               isOn: Binding(get: { model.ticked.contains(room.id) && needs.isEmpty }, set: { model.setRoom(room.id, on: $0) }))
                            .toggleStyle(.kith).disabled(!needs.isEmpty)
                    }
                }
            }
            if !model.projects.isEmpty {
                CardSection("Rooms") {
                    ForEach(model.projects, id: \.self) { project in
                        Toggle(project, isOn: Binding(get: { model.projectsTicked.contains(project) }, set: { model.setProject(project, on: $0) }))
                            .toggleStyle(.kith)
                    }
                }
            }
            if !model.online {
                Text("This computer is offline. Connect to it to make an invite.").font(.footnote).foregroundStyle(Palette.secondary)
            }
            if let error = model.people.error {
                Text(error).font(.footnote).foregroundStyle(Palette.danger)
            }
            Button { Task { await model.create() } } label: {
                ZStack {
                    Text(model.joined ? "Share" : "Create invite").opacity(model.people.busy ? 0 : 1)
                    if model.people.busy { Spinner(size: 18) }
                }
                .frame(maxWidth: .infinity, minHeight: 28)
            }
            .buttonStyle(.primary)
            .disabled(!model.canCreate)
        }
    }

    private func agentRow(_ agent: RoomAgent) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Toggle(agent.name, isOn: Binding(get: { model.roles[agent.id] != nil }, set: { model.setAgent(agent.id, on: $0) }))
                .toggleStyle(.kith)
                .disabled(model.isHeld(agent.id))
            if let role = model.roles[agent.id] {
                SegmentedChoice(selection: Binding(get: { role }, set: { model.roles[agent.id] = $0 }),
                                options: InviteRole.allCases.map { ($0, $0.levelTitle) })
                if model.isHeld(agent.id) {
                    Text("\(model.who) has \(agent.name) already.").font(.footnote).foregroundStyle(Palette.secondary)
                }
            }
        }
    }
}

/// The link, to copy or share, and a QR code; the words the owner passes on with it.
public struct InviteLinkView: View {
    let invite: CreatedInvite
    let name: String
    /// "Mia joins Studio when Mia accepts." when the invite adds the person to rooms.
    var joins: String?
    @State private var copied = false

    public init(invite: CreatedInvite, name: String, joins: String? = nil) {
        self.invite = invite
        self.name = name
        self.joins = joins
    }

    public var body: some View {
        CardForm {
            CardSection("Invite for \(name)", footer: "Single use, expires in 24 hours. Send it to them; they sign in with Google, Apple or GitHub.") {
                Text(invite.link)
                    .font(.system(.footnote, design: .monospaced))
                    .foregroundStyle(Palette.text)
                    .textSelection(.enabled)
                    .lineLimit(4)
                    .truncationMode(.middle)
                if let joins { Text(joins).foregroundStyle(Palette.text) }
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
