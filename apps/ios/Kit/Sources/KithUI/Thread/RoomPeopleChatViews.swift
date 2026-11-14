import KithKit
import SwiftUI

/// "Ben joined · sees from here" (the first join adds the rule line), "Ben left": quiet dividers in the time-separator style.
struct PersonLine: View {
    let entry: Entry
    @Environment(BotStore.self) private var model

    var body: some View {
        let lines = RoomPeopleLogic.joinLines(entry, firstJoin: RoomPeopleLogic.isFirstJoin(entry, in: model.allEntries(entry.botId)))
        VStack(spacing: 4) {
            HStack(spacing: 10) {
                Rectangle().fill(Palette.border).frame(height: 1)
                Text(lines[0]).font(.caption2).foregroundStyle(Palette.muted)
                Rectangle().fill(Palette.border).frame(height: 1)
            }
            if lines.count > 1 { Text(lines[1]).font(.caption2).foregroundStyle(Palette.muted) }
        }
        .accessibilityElement(children: .combine)
    }
}

/// "From before Ben joined" under a message Ben can't see, only below his join line (the divider explains the rest).
struct HiddenMarker: View {
    let entry: Entry
    @Environment(BotStore.self) private var model
    @State private var help = false

    var body: some View {
        let names = RoomPeopleLogic.hiddenFromNames(entry, stays: model.roomStays[entry.botId] ?? [], nameOf: { model.personName($0) })
        if !names.isEmpty {
            VStack(alignment: .leading, spacing: 2) {
                Button { withAnimation(Motion.fade) { help.toggle() } } label: {
                    HStack(spacing: 4) {
                        Image(systemName: "eye.slash").font(.system(size: 11))
                        Text(RoomPeopleLogic.hiddenMarkerText(names)).font(.system(size: 12))
                    }
                    .foregroundStyle(Palette.muted).frame(minHeight: 44, alignment: .leading).contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel("\(RoomPeopleLogic.joined(names)) can't see this message.")
                if help {
                    Text(RoomPeopleLogic.hiddenMarkerHelp(names, agent: entry.kind == "agent" ? model.authorName(entry.data.author) : nil))
                        .font(.system(size: 12)).foregroundStyle(Palette.muted)
                }
            }
        }
    }
}

/// A room turn that didn't answer: one quiet line, and Send again for the viewer's own message (back in the box, not sent).
struct UnansweredLine: View {
    let entry: Entry
    @Environment(BotStore.self) private var model

    var body: some View {
        let trigger = RoomPeopleLogic.unansweredTrigger(entry, in: model.allEntries(entry.botId))
        HStack(spacing: 8) {
            if let bot = entry.data.author.flatMap({ model.bots[$0] }) { CharacterAvatar(bot: bot, size: 18, animated: false) }
            Text(entry.data.text ?? "").font(.system(size: 13)).foregroundStyle(Palette.muted).fixedSize(horizontal: false, vertical: true)
            if let trigger {
                Button { model.sendAgain(trigger.data.text ?? "", in: entry.botId) } label: {
                    Text("Send again").font(.system(size: 14, weight: .semibold)).foregroundStyle(Palette.ink)
                        .padding(.horizontal, 12).frame(minHeight: 44)
                        .overlay(Capsule().strokeBorder(Palette.border, lineWidth: 1))
                }
                .buttonStyle(.plain)
            }
        }
    }
}

/// The waiting notice, as a pointer into the owner's 1:1 with the agent (the card counts there, once). Owner only.
struct WaitingPointer: View {
    let entry: Entry
    let agent: String
    @Environment(BotStore.self) private var model

    var body: some View {
        let bot = model.bots.values.first { !$0.isGroup && ($0.name == agent || $0.hqAgent == agent) }
        let open = bot.map { b in model.allEntries(b.id).contains { $0.kind == "permission" && $0.data.room == entry.botId && $0.data.status == "pending" } } ?? false
        HStack(spacing: 8) {
            if let bot { CharacterAvatar(bot: bot, size: 18, animated: false) }
            if open, let bot {
                Text("\(agent) is waiting for you").font(.system(size: 13)).foregroundStyle(Palette.ink)
                Button { model.selection = bot.id } label: {
                    HStack(spacing: 2) { Text("Open").font(.system(size: 14, weight: .semibold)); Image(systemName: "chevron.right").font(.system(size: 10)) }
                        .foregroundStyle(Palette.ink).frame(minHeight: 44).contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            } else {
                Text("\(agent) · you answered").font(.system(size: 13)).foregroundStyle(Palette.muted)
            }
        }
    }
}

/// In the 1:1: where a room turn's card came from.
struct CardOrigin: View {
    let entry: Entry
    @Environment(BotStore.self) private var model

    var body: some View {
        if let room = entry.data.room.flatMap({ model.bots[$0] }) {
            Button { model.selection = room.id } label: {
                HStack(spacing: 5) {
                    Image(systemName: "bubble.left.and.bubble.right").font(.system(size: 11))
                    Text("From \(room.name) · runs for everyone in \(room.name)").font(.system(size: 12))
                }
                .foregroundStyle(Palette.muted).frame(minHeight: 44, alignment: .leading).contentShape(Rectangle())
            }
            .buttonStyle(.plain)
        }
    }
}

/// Above the owner's box: which agents won't answer in this room, and why (a state, not a question).
struct WontAnswerLine: View {
    let group: Bot
    let fix: () -> Void
    @Environment(BotStore.self) private var model

    var body: some View {
        if let line = model.wontAnswerLine(in: group) {
            HStack(spacing: 8) {
                Image(systemName: "person.crop.circle.badge.xmark").foregroundStyle(Palette.muted)
                Text(line).font(.system(size: 13)).foregroundStyle(Palette.ink).fixedSize(horizontal: false, vertical: true)
                Spacer(minLength: 4)
                Button(action: fix) { Text("Fix").font(.system(size: 15, weight: .semibold)).foregroundStyle(Palette.accent).frame(minWidth: 44, minHeight: 44) }
                    .buttonStyle(.plain)
            }
            .padding(.horizontal, 12)
            .background(Palette.surface, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
            .accessibilityElement(children: .contain)
        }
    }
}
