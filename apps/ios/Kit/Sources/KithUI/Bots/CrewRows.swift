import KithKit
import SwiftUI

/// Maps the roster onto what Crew shows: rows, their order keys and what the dock searches.
public enum CrewRows {
    /// One row per room. `team` is a room on a computer joined by invite: shared with the person who owns it.
    @MainActor public static func model(for item: RosterItem, store: BotStore, team: Bool, asleep: Bool = false) -> CrewRowModel {
        let bot = item.bot
        let face = CrewFace(bot: bot)
        let shape: CrewRowModel.Shape
        var person: String?
        if bot.isGroup {
            let members = store.members(of: bot).map(CrewFace.init(bot:))
            shape = .group(members.first ?? face, members.dropFirst().first ?? face, personId: bot.id)
        } else if team {
            shape = .team(face, personId: store.hostName)
            person = store.hostName
        } else {
            shape = .agent(face)
        }
        let waiting = BoardModel.waitingCount(in: store.entries[bot.id] ?? [])
        let state: BotAvatarState = asleep ? .asleep : bot.needsInput ? .needsYou(max(waiting, 1)) : bot.isWorking ? .working : .idle
        let trailing: String? = asleep ? "asleep" : bot.isWorking && !bot.needsInput ? "working"
            : bot.lastAt > 0 ? RelativeTime.day(Date(milliseconds: bot.lastAt)) : nil
        return CrewRowModel(id: rowId(item), shape: shape, name: bot.name, withPerson: person, trailing: trailing,
                            status: status(bot, store: store), count: waiting, unread: bot.unread > 0, state: state, tag: bot.botGuest?.tag, botProvider: bot.botGuest?.provider)
    }

    public static func rowId(_ item: RosterItem) -> String { "\(item.ref.computerId)/\(item.bot.id)" }

    @MainActor static func status(_ bot: Bot, store: BotStore) -> String {
        let using = store.screen?.agentBot == bot.id ? "Using the computer · " : ""
        if bot.needsInput { return using + (bot.activity.isEmpty ? "Needs your approval" : bot.activity) }
        if bot.isWorking { return using + (bot.activity.isEmpty ? "Working…" : bot.activity) }
        if bot.status == "error" { return using + MessagePreview.text(bot.lastMessage ?? "Something went wrong") }
        return using + MessagePreview.text(bot.lastMessage ?? (bot.isGroup ? "\(bot.members.count) agents" : "\(store.backendName(bot.backend)) · \(bot.folderName)"))
    }

    public static func orderItem(_ item: RosterItem, computerIndex: Int = 0) -> CrewOrderItem {
        CrewOrderItem(id: rowId(item), computer: computerIndex, pinned: item.bot.pinned, position: item.bot.position, lastAt: item.bot.lastAt,
                      needsYou: item.bot.needsInput, working: item.bot.isWorking)
    }

    @MainActor public static func searchEntry(_ item: RosterItem, store: BotStore, team: Bool) -> CrewSearchEntry {
        let bot = item.bot
        let people = bot.isGroup ? store.members(of: bot).map(\.name) : team ? [store.hostName] : []
        // Only the newest messages: the box searches on every keystroke.
        let messages = store.chat(bot.id).reversed().lazy.filter(\.isChat).prefix(200).compactMap { entry in
            entry.data.text.flatMap { $0.isEmpty ? nil : CrewSearchMessage(id: entry.id, text: $0) }
        }
        return CrewSearchEntry(id: rowId(item), kind: bot.isGroup ? .group : team ? .team : .agent, name: bot.name, people: people,
                               status: status(bot, store: store),
                               extras: [bot.isGroup ? "group" : store.backendName(bot.backend), bot.isGroup ? "" : bot.folderName],
                               messages: Array(messages))
    }
}
