import KithKit
import KithUI
import SwiftUI

/// Board, the second page of home: what waits on you (answerable here, with the chat's own call), what is at work,
/// what finished today, where a chat started over, the projects the crew works in, and what it is waiting on. Under
/// the same morning sky as Crew, with no title of its own: the top bar's switch says where you are.
struct BoardView: View {
    /// Already filtered by the dock's box.
    let model: BoardModel
    let searching: Bool
    /// Computer names show on items only when there is more than one.
    let showsHosts: Bool
    let store: (ComputerID) -> BotStore?
    let open: (BoardTarget) -> Void
    let showComputers: () -> Void

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 0) {
                if model.isEmpty {
                    empty
                } else {
                    needsYou
                    working
                    done
                    leftOff
                    projects
                    waits
                }
            }
            .padding(.bottom, 16)
        }
        .scrollDismissesKeyboard(.immediately)
    }

    // MARK: sections

    @ViewBuilder private var needsYou: some View {
        if !model.needsYou.isEmpty {
            header("Needs you", count: model.needsYou.count)
            ForEach(model.needsYou) { item in
                VStack(alignment: .leading, spacing: 6) {
                    if showsHosts || item.trouble != nil {
                        HStack(spacing: 8) {
                            if showsHosts { Text(item.hostName).kithText(.secondary).foregroundStyle(Palette.muted) }
                            if let trouble = item.trouble { chip(trouble) }
                        }
                    }
                    // The same call and the same pending state as the chat's card: a second tap while one is on its way
                    // does nothing, and the card goes when the host marks it answered, wherever that was.
                    let live = store(item.target.computerId)
                    if let bot = item.botRequest {
                        DecisionCard(item.card, answering: live?.botGuests.deciding[bot.botId]) { option in
                            if let guests = live?.botGuests { Task { await guests.decide(bot, option: option) } }
                        }
                    } else if item.opensChat {
                        // Never a field here: the key is typed on the agent's own card in the chat.
                        Button { open(item.target) } label: {
                            HStack(spacing: 10) {
                                Image(systemName: "key").foregroundStyle(Palette.muted)
                                VStack(alignment: .leading, spacing: 2) {
                                    if let heading = item.card.heading { Text(heading).kithText(.label).foregroundStyle(Palette.muted) }
                                    Text(item.card.question).kithText(.body).fontWeight(.semibold).foregroundStyle(Palette.ink)
                                }
                                Spacer(minLength: 0)
                                Image(systemName: "chevron.right").font(.footnote).foregroundStyle(Palette.muted)
                            }
                            .padding(14)
                            .frame(maxWidth: .infinity, minHeight: 44, alignment: .leading)
                            .background(AgentTint.resolve(item.color).b.opacity(0.32), in: RoundedRectangle(cornerRadius: KithRadius.card, style: .continuous))
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .accessibilityHint("Opens the chat")
                    } else if let entry = item.entry {
                        DecisionCard(item.card, tint: AgentTint.resolve(item.color), answering: live?.answering[entry.id]) {
                            live?.respond(entry, option: $0)
                        }
                    }
                }
                .padding(.horizontal, KithSpacing.gutter)
                .padding(.vertical, 6)
                .transition(.opacity)
            }
        }
    }

    @ViewBuilder private var working: some View {
        if !model.working.isEmpty {
            header("Working", count: model.working.count)
            ForEach(model.working) { item in workRow(item, trailing: nil, state: .working) }
        }
    }

    @ViewBuilder private var done: some View {
        if !model.doneToday.isEmpty {
            header("Done today", count: model.doneToday.count)
            ForEach(model.doneToday) { item in
                workRow(item, trailing: RelativeTime.day(Date(milliseconds: item.at)), state: .idle)
            }
        }
    }

    @ViewBuilder private var leftOff: some View {
        if !model.leftOff.isEmpty {
            ForEach(model.leftOff) { item in
                Button { open(item.target) } label: {
                    VStack(alignment: .leading, spacing: 6) {
                        Text(showsHosts ? "\(item.botName), on \(item.hostName)" : item.botName)
                            .kithText(.secondary).foregroundStyle(Palette.muted).lineLimit(1)
                        HandoffCardView(item.card)
                    }
                    .padding(.horizontal, KithSpacing.gutter)
                    .padding(.top, 14)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityElement(children: .combine)
                .accessibilityHint("Opens the chat")
            }
        }
    }

    @ViewBuilder private var projects: some View {
        if !model.projects.isEmpty {
            header("Projects", count: model.projects.count)
            ForEach(model.projects) { project in
                Button { open(project.target) } label: {
                    HStack(spacing: 14) {
                        Image(systemName: "shippingbox")
                            .font(.system(size: 18, weight: .regular)).foregroundStyle(Palette.muted)
                            .frame(width: KithSize.botAvatar)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(project.name).kithText(.agentName).foregroundStyle(Palette.ink).lineLimit(1)
                            Text(projectDetail(project)).kithText(.body).foregroundStyle(Palette.mutedOnSky).lineLimit(1)
                        }
                        Spacer(minLength: 0)
                    }
                    .padding(.horizontal, KithSpacing.gutter)
                    .padding(.vertical, KithSpacing.rowV)
                    .frame(minHeight: 44)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel("\(project.name), \(projectDetail(project))")
                .accessibilityHint("Opens its most recent agent")
                separator
            }
        }
    }

    @ViewBuilder private var waits: some View {
        if !model.waits.isEmpty {
            header("Waits", count: model.waits.count)
            ForEach(model.waits) { wait in
                Button {
                    if let target = wait.target { open(target) } else { showComputers() }
                } label: {
                    HStack(spacing: 14) {
                        Image(systemName: icon(wait))
                            .font(.system(size: 18, weight: .regular)).foregroundStyle(Palette.muted)
                            .frame(width: KithSize.botAvatar)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(wait.title).kithText(.agentName).foregroundStyle(Palette.ink).lineLimit(1)
                            Text(wait.detail).kithText(.body).foregroundStyle(Palette.mutedOnSky).lineLimit(2)
                        }
                        Spacer(minLength: 6)
                        if case .computer(let trouble) = wait.kind { chip(trouble) }
                    }
                    .padding(.horizontal, KithSpacing.gutter)
                    .padding(.vertical, KithSpacing.rowV)
                    .frame(minHeight: 44)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(waitLabel(wait))
                .accessibilityHint(wait.target == nil ? "Opens Computers" : "Opens the chat")
                separator
            }
        }
    }

    // MARK: empty

    private var empty: some View {
        VStack(spacing: 10) {
            if searching {
                Text("No match. Send starts a new session with an agent you pick.")
                    .kithText(.body).foregroundStyle(Palette.muted).multilineTextAlignment(.center)
            } else {
                Image(systemName: "checkmark.circle").font(.system(size: 28, weight: .regular)).foregroundStyle(Palette.muted)
                    .accessibilityHidden(true)
                Text("Nothing waiting on you").kithText(.topBar).foregroundStyle(Palette.ink)
                Text("Questions from your agents, and the work in flight, show up here.")
                    .kithText(.body).foregroundStyle(Palette.muted).multilineTextAlignment(.center)
            }
        }
        .frame(maxWidth: .infinity)
        .padding(.horizontal, 32)
        .padding(.vertical, 64)
        .accessibilityElement(children: .combine)
    }

    // MARK: pieces

    private func header(_ title: String, count: Int) -> some View {
        Text("\(title.uppercased()) · \(count)")
            .kithText(.label).fontWeight(.semibold)
            .foregroundStyle(Palette.muted)
            .padding(.horizontal, KithSpacing.gutter)
            .padding(.top, 16)
            .padding(.bottom, 4)
            .accessibilityLabel("\(title), \(count)")
            .accessibilityAddTraits(.isHeader)
    }

    private var separator: some View {
        Rectangle().fill(Palette.ink.opacity(0.08)).frame(height: 0.5)
            .padding(.leading, KithSpacing.gutter + KithSize.botAvatar + 14)
    }

    private func chip(_ trouble: BoardTrouble) -> some View {
        StatusChip(trouble == .unreachable ? .macUnreachable : .catchingUp)
    }

    private func workRow(_ item: BoardWork, trailing: String?, state: BotAvatarState) -> some View {
        let detail = showsHosts ? "\(item.bot.name) · \(item.hostName) · \(item.detail)" : "\(item.bot.name) · \(item.detail)"
        return VStack(alignment: .leading, spacing: 0) {
            Button { open(item.target) } label: {
                CrewRow(CrewRowModel(id: item.id, shape: .agent(CrewFace(bot: item.bot)), name: item.title, trailing: trailing,
                                     status: detail, state: state))
            }
            .buttonStyle(.plain)
            if let trouble = item.trouble {
                chip(trouble)
                    .padding(.leading, KithSpacing.gutter + KithSize.botAvatar + 14)
                    .padding(.bottom, 6)
            }
            separator
        }
    }

    private func projectDetail(_ project: BoardProject) -> String {
        let agents = project.agents == 1 ? "1 agent" : "\(project.agents) agents"
        return project.working > 0 ? "\(agents) · \(project.working) working" : agents
    }

    private func icon(_ wait: BoardWait) -> String {
        switch wait.kind {
        case .queued: "tray.full"
        case .computer(let trouble): trouble == .unreachable ? "wifi.slash" : "arrow.triangle.2.circlepath"
        }
    }

    private func waitLabel(_ wait: BoardWait) -> String {
        switch wait.kind {
        case .queued: "\(wait.title), \(wait.detail)"
        case .computer(let trouble): "\(wait.title), \(trouble == .unreachable ? "Mac unreachable" : "catching up")"
        }
    }
}
