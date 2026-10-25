import KithKit
import SwiftUI

/// A task in the chat: inline (its first message, the thread under it, the bot at work) while it's going, a card once
/// it's idle. The card opens it; the open task folds back. An older task goes on in its thread (`openThread`).
struct TaskRow: View {
    let root: Entry
    let bot: Bot
    let newest: Bool
    /// What the user picked on this device: open or folded; nil follows the task.
    @Binding var choice: Bool?
    let openTrace: () -> Void
    let openThread: () -> Void
    @Environment(BotStore.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        // Ticks so an idle task folds on its own.
        TimelineView(.periodic(from: .now, by: 30)) { context in
            let replies = model.replies(bot.id, root: root.id)
            let status = Tasks.status(of: root, bot: bot, replies: replies)
            let now = Int64(context.date.timeIntervalSince1970 * 1000)
            // A question that waits opens its thread even if it was folded by hand.
            let open = ThreadFeed.hasQuestion(replies, held: root.data.thread?.isHeldElsewhere == true) || Tasks.isOpen(status, newest: newest, lastAt: Tasks.lastAt(root), now: now, choice: choice)
            // An older reply can be missing when only live ones arrived: load the whole thread.
            let missing = open && ThreadFeed.isMissingReplies(replies, summary: root.data.thread)
            thread(replies, status, open: open, now: context.date)
                .task(id: missing) {
                    if missing { await model.loadThread(bot.id, root: root.id) }
                }
        }
    }

    /// Your message as a normal bubble; its thread hangs under it as the chip, and unfolds in place.
    private func thread(_ replies: [Entry], _ status: TaskStatus, open: Bool, now: Date) -> some View {
        let tint = AgentTint.resolve(bot.avatarColor)
        let summary = root.data.thread
        let chip = ThreadFeed.chip(face: CrewFace(bot: bot), tint: tint, count: summary?.count ?? 0, unread: summary?.unread,
                                   lastAt: Tasks.lastAt(root), place: model.placeFolder(bot.id, thread: root.id),
                                   yourTurn: ThreadFeed.hasQuestion(replies, held: summary?.isHeldElsewhere == true),
                                   summary: summary, mac: model.hostName, now: now)
        return VStack(alignment: .trailing, spacing: 4) {
            ChatRow(entry: root, groupStart: true, chat: bot, openTrace: openTrace)
            HStack(spacing: 4) {
                if atWork(status) {
                    IconButton("Stop this task", systemImage: "stop.fill") { model.stop(bot.id, thread: root.id) }
                }
                ThreadChipView(chip, isOpen: open, onToggle: {
                    withAnimation(Motion.reduced(Motion.layout, reduceMotion)) { choice = !open }
                }, onOpenThread: openThread)
            }
            if open { openReplies(replies, status, tint: tint) }
        }
        .padding(.vertical, 4)
    }

    /// The replies under a thin line in the agent's colour: bubbles, tool rows, decision cards, left-off cards.
    private func openReplies(_ replies: [Entry], _ status: TaskStatus, tint: AgentTint) -> some View {
        HStack(alignment: .top, spacing: 10) {
            RoundedRectangle(cornerRadius: 1).fill(tint.a).frame(width: 2)
            VStack(alignment: .leading, spacing: 8) {
                Text(Tasks.label(status, replies: replies)).kithText(.label).foregroundStyle(Palette.muted)
                ForEach(ThreadFeed.replies(replies)) { reply in
                    switch reply {
                    case .entry(let e, let groupStart):
                        ChatRow(entry: e, groupStart: groupStart, chat: bot, openTrace: openTrace)
                    case .separator:
                        EmptyView()
                    case .tool(let e):
                        ToolCallRow(ThreadFeed.toolRow(e))
                    case .earlier(let count, _):
                        Button(action: openTrace) {
                            Text(count == 1 ? "1 earlier step" : "\(count) earlier steps")
                                .kithText(.secondary).foregroundStyle(Palette.muted)
                                .frame(minHeight: 32, alignment: .leading)
                        }
                        .buttonStyle(.plain)
                    }
                }
                if atWork(status) && !model.isOffline {
                    WorkingIndicator(bot: bot, thinking: model.currentThinking(bot.id, thread: root.id))
                        .padding(.top, 6)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(.top, 4)
        // Its thread is on screen: the bot's answers here are read.
        .readingConversation(bot.id, thread: root.id)
        .transition(.opacity)
    }

    /// Whether a lane is at work on it: a message parked at its agent's monthly limit needs you with nothing working.
    private func atWork(_ status: TaskStatus) -> Bool {
        status == .working || (status == .needsYou && bot.isWorking(in: bot.id, thread: root.id))
    }
}
