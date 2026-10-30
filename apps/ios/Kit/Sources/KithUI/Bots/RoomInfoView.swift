import KithKit
import SwiftUI

/// A group room's info: the people in it, its agents and the bots someone brought in, and the way to add more.
/// Adding a bot is the owner's yes in a sheet, not an alert: the card says what the bot will see.
public struct RoomInfoView: View {
    @Environment(BotStore.self) private var model
    @Environment(\.dismissModal) private var dismiss
    let groupId: String
    @State private var adding: Bool
    @State private var editing = false
    @State private var removing: String?
    @State private var error: String?

    public init(groupId: String, adding: Bool = false) {
        self.groupId = groupId
        _adding = State(initialValue: adding)
    }

    private var group: Bot? { model.bots[groupId] }

    public var body: some View {
        VStack(spacing: 0) {
            ModalHeader(group?.name ?? "Room") {
                Button("Edit") { editing = true }
                    .font(.body.weight(.medium)).foregroundStyle(Palette.accent)
            }
            ScrollView {
                VStack(alignment: .leading, spacing: 22) {
                    people
                    members
                    if let error { Text(error).kithText(.secondary).foregroundStyle(Palette.danger) }
                }
                .padding(.horizontal, 20)
                .padding(.bottom, 24)
            }
        }
        .background(Palette.background)
        .kithSheet(isPresented: $adding) { if let group { AddToRoomSheet(group: group) } }
        .kithSheet(isPresented: $editing) { GroupEditorView(group: group) }
    }

    // MARK: sections

    private func section(_ title: String, action: (() -> Void)? = nil, actionLabel: String = "", @ViewBuilder content: () -> some View) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text(title.uppercased()).font(.system(size: 13, weight: .semibold)).tracking(0.8).foregroundStyle(Palette.muted)
                Spacer()
                if let action {
                    Button(action: action) {
                        Text(actionLabel).font(.system(size: 16, weight: .semibold)).foregroundStyle(Palette.accent)
                            .frame(minHeight: 44).contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                }
            }
            .padding(.leading, 4)
            content()
        }
    }

    private var people: some View {
        section("People") {
            card { row(face: Circle().fill(LinearGradient(colors: [Color(hex: 0xF9C3D3), Color(hex: 0xF4A6C4)], startPoint: .top, endPoint: .bottom)).frame(width: 40, height: 40),
                       title: Text("You").kithText(.topBar), detail: "Owner", trailing: EmptyView()) }
        }
    }

    private var members: some View {
        section("Agents and bots", action: model.isOwner ? { adding = true } : nil, actionLabel: "Add") {
            if let group {
                let agents = model.members(of: group)
                let bots = model.guestMembers(of: group)
                card {
                    ForEach(agents) { agent in
                        row(face: CharacterAvatar(bot: agent, size: 40, animated: false),
                            title: Text(agent.name).kithText(.agentName), detail: "Answers on its turn", trailing: EmptyView())
                        if agent.id != agents.last?.id || !bots.isEmpty { Divider().padding(.leading, 64) }
                    }
                    ForEach(bots) { bot in
                        row(face: RoomBotFace(provider: bot.provider, name: bot.name, size: 40),
                            title: HStack(spacing: 8) { Text(bot.name).kithText(.topBar); BotTag() },
                            detail: "Your bot · runs on \(bot.providerTitle), outside this Mac",
                            trailing: model.isOwner ? AnyView(removeButton(bot)) : AnyView(EmptyView()))
                        if bot.id != bots.last?.id { Divider().padding(.leading, 64) }
                    }
                }
                if let bot = bots.first {
                    let when = joined(bot).map { ", \($0)" } ?? ""
                    Text("\(bot.name) sees what is said here from when you added it\(when). It never opens a run: only a message of yours gives the room turns.")
                        .kithText(.secondary).foregroundStyle(Palette.muted).padding(.horizontal, 4)
                }
            }
        }
    }

    private func removeButton(_ bot: BotGuest) -> some View {
        Button { remove(bot) } label: {
            Group {
                if removing == bot.botId { Spinner(size: 14) } else { Text("Remove").font(.system(size: 16, weight: .semibold)) }
            }
            .foregroundStyle(Palette.danger).frame(minWidth: 60, minHeight: 44).contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(removing != nil)
        .accessibilityLabel("Remove \(bot.name) from this room")
    }

    private func card(@ViewBuilder _ content: () -> some View) -> some View {
        VStack(spacing: 0) { content() }
            .background(Palette.background, in: RoundedRectangle(cornerRadius: KithRadius.card, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: KithRadius.card, style: .continuous).strokeBorder(Palette.border, lineWidth: 1))
    }

    private func row(face: some View, title: some View, detail: String, trailing: some View) -> some View {
        HStack(spacing: 14) {
            face.frame(width: 40, height: 40)
            VStack(alignment: .leading, spacing: 2) {
                title.foregroundStyle(Palette.ink).lineLimit(1)
                Text(detail).kithText(.secondary).foregroundStyle(Palette.muted).fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 8)
            trailing
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .frame(minHeight: 56)
    }

    /// When the host wrote the join line: "today 11:02".
    private func joined(_ bot: BotGuest) -> String? {
        let line = model.allEntries(groupId).last { $0.data.notice == "botJoined" && ($0.data.text ?? "").hasPrefix(bot.name) }
        return line.map { RelativeTime.separator($0.date).lowercased() }
    }

    private func remove(_ bot: BotGuest) {
        guard let group, removing == nil else { return }
        removing = bot.botId
        error = nil
        Task {
            do {
                var draft = GroupDraft(group)
                draft.members.removeAll { $0 == bot.botId }
                try await model.updateGroup(draft)
            } catch {
                withAnimation(Motion.fade) { self.error = error.localizedDescription }
            }
            removing = nil
        }
    }
}

/// Room info > Agents and bots > Add: the owner's agents, then their bots. A bot that can't join says why. Picking one
/// opens the card that says what it will see; its button is the yes.
struct AddToRoomSheet: View {
    @Environment(BotStore.self) private var model
    @Environment(\.dismissModal) private var dismiss
    let group: Bot
    @State private var pick: String?
    @State private var saving = false
    @State private var error: String?

    init(group: Bot, pick: String? = nil) {
        self.group = group
        _pick = State(initialValue: pick)
    }

    private var agents: [Bot] { model.roster.filter { !$0.isGroup && $0.botGuest == nil && !group.members.contains($0.id) } }
    private var bots: [BotGuest] { BotGuest.roomCandidates(model.botGuests.mine).filter { !group.members.contains($0.botId) } }
    private var pickedAgent: Bot? { agents.first { $0.id == pick } }
    private var pickedBot: BotGuest? { bots.first { $0.botId == pick && $0.roomBlock == nil } }

    var body: some View {
        VStack(spacing: 0) {
            ModalHeader("Add to \(group.name)") {}
            ScrollView {
                VStack(alignment: .leading, spacing: 22) {
                    if !agents.isEmpty { list("Your agents") { ForEach(agents) { agent in agentRow(agent) } } }
                    if !bots.isEmpty { list("Your bots") { ForEach(bots) { bot in botRow(bot) } } }
                    if agents.isEmpty && bots.isEmpty {
                        Text("Everyone you can add is already in this room.").kithText(.body).foregroundStyle(Palette.muted)
                    }
                    if let bot = pickedBot { botCard(bot) } else if let agent = pickedAgent { agentCard(agent) }
                    if let error { Text(error).kithText(.secondary).foregroundStyle(Palette.danger) }
                }
                .padding(.horizontal, 20)
                .padding(.bottom, 24)
            }
        }
        .background(Palette.background)
        .animation(Motion.layout, value: pick)
    }

    private func list(_ title: String, @ViewBuilder content: () -> some View) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title.uppercased()).font(.system(size: 13, weight: .semibold)).tracking(0.8).foregroundStyle(Palette.muted).padding(.leading, 4)
            VStack(spacing: 0) { content() }
                .overlay(RoundedRectangle(cornerRadius: KithRadius.card, style: .continuous).strokeBorder(Palette.border, lineWidth: 1))
        }
    }

    private func choice(face: some View, title: some View, detail: String?, selected: Bool, enabled: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            HStack(spacing: 14) {
                face.frame(width: 40, height: 40)
                VStack(alignment: .leading, spacing: 2) {
                    title.lineLimit(1)
                    if let detail { Text(detail).kithText(.secondary).fixedSize(horizontal: false, vertical: true) }
                }
                .foregroundStyle(enabled ? Palette.ink : Palette.muted)
                Spacer(minLength: 8)
                if enabled {
                    ZStack {
                        Circle().strokeBorder(Palette.border, lineWidth: 1.5).frame(width: 28, height: 28)
                        if selected {
                            Circle().fill(Palette.ink).frame(width: 28, height: 28)
                            Image(systemName: "checkmark").font(.system(size: 12, weight: .bold)).foregroundStyle(Palette.background)
                        }
                    }
                }
            }
            .padding(.horizontal, 14).padding(.vertical, 10)
            .frame(minHeight: 56)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(!enabled)
        .accessibilityAddTraits(selected ? .isSelected : [])
    }

    private func agentRow(_ agent: Bot) -> some View {
        choice(face: CharacterAvatar(bot: agent, size: 40, animated: false), title: Text(agent.name).kithText(.agentName),
               detail: nil, selected: pick == agent.id, enabled: true) { pick = pick == agent.id ? nil : agent.id }
    }

    private func botRow(_ bot: BotGuest) -> some View {
        let block = bot.roomBlock
        return choice(face: RoomBotFace(provider: bot.provider, name: bot.name, size: 40).opacity(block == nil ? 1 : 0.45),
                      title: HStack(spacing: 8) { Text(bot.name).kithText(.topBar); BotTag() },
                      detail: block ?? "Your bot · on \(bot.providerTitle)", selected: pick == bot.botId, enabled: block == nil) {
            pick = pick == bot.botId ? nil : bot.botId
        }
    }

    // MARK: the yes

    private func botCard(_ bot: BotGuest) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .top, spacing: 12) {
                RoomBotFace(provider: bot.provider, name: bot.name, size: 40)
                Text("\(bot.name) will see this room from now on.").kithText(.topBar).foregroundStyle(Palette.ink)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Text("It reads what is said here and answers on its turn or when someone @-mentions it. Its replies use the turns you give.")
                .kithText(.body).foregroundStyle(Palette.muted).fixedSize(horizontal: false, vertical: true)
            fact("arrow.up.right.square", "What it reads goes to \(bot.providerTitle), outside this Mac.")
            fact("bell", bot.isWoken ? "Woken on its turn through its webhook · \(bot.waitWords)." : "No wake is set: its turn counts as a pass unless it posts first.")
            buttons("Add \(bot.name)") { add(bot.botId) }
        }
        .cardSurface()
    }

    private func agentCard(_ agent: Bot) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("\(agent.name) joins this room.").kithText(.topBar).foregroundStyle(Palette.ink)
            Text("It answers on its turn or when someone @-mentions it.").kithText(.body).foregroundStyle(Palette.muted)
            buttons("Add \(agent.name)") { add(agent.id) }
        }
        .cardSurface()
    }

    private func fact(_ icon: String, _ text: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            Image(systemName: icon).font(.system(size: 13)).foregroundStyle(Palette.muted).frame(width: 18)
            Text(text).kithText(.body).foregroundStyle(Palette.ink).fixedSize(horizontal: false, vertical: true)
        }
    }

    private func buttons(_ title: String, add: @escaping () -> Void) -> some View {
        HStack(spacing: 10) {
            Button(action: add) {
                Group { if saving { Spinner(size: 14) } else { Text(title).font(.system(size: 17, weight: .semibold)) } }
                    .foregroundStyle(Palette.background).frame(maxWidth: .infinity, minHeight: 48)
                    .background(Palette.ink, in: Capsule())
            }
            .buttonStyle(.plain).disabled(saving)
            Button { pick = nil } label: {
                Text("Cancel").font(.system(size: 17, weight: .semibold)).foregroundStyle(Palette.ink)
                    .frame(maxWidth: .infinity, minHeight: 48)
                    .overlay(Capsule().strokeBorder(Palette.border, lineWidth: 1))
            }
            .buttonStyle(.plain).disabled(saving)
        }
        .padding(.top, 4)
    }

    /// The existing group edit does it: the room's members, plus this one (as `kith group add-bot` does).
    private func add(_ id: String) {
        guard !saving else { return }
        saving = true
        error = nil
        Task {
            do {
                var draft = GroupDraft(group)
                if !draft.members.contains(id) { draft.members.append(id) }
                try await model.updateGroup(draft)
                dismiss()
            } catch {
                withAnimation(Motion.fade) { self.error = error.localizedDescription }
            }
            saving = false
        }
    }
}

private extension View {
    func cardSurface() -> some View {
        padding(16)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Palette.surface, in: RoundedRectangle(cornerRadius: KithRadius.card, style: .continuous))
            .transition(.opacity)
    }
}
