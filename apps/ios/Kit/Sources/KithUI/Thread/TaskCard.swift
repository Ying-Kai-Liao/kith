import KithKit
import SwiftUI

// A task in the flat chat (designs/2026-10-30-task-cards.md): its first message and its thread as one card. While it
// works the card is open, with a spine, every row and the live line (Stop sits there); once it stops it folds to the
// first message, a line saying what the fold hides, and the last reply. A question that waits on you shows on the
// card, answerable there, and is the only card with a fill. Reply points the chat's one box at this task.

/// The task cards on screen, kept outside the list's own state so a card scrolling in or out redraws the box only.
@Observable final class VisibleTasks {
    var ids: Set<String> = []
    /// The list has had time to lay out: before that, no card counts as off screen.
    var ready = false
}

extension ThreadView {
    func taskCard(root: Entry, thread: [Entry]) -> some View {
        let bot = self.bot
        let status = bot.map { Tasks.status(of: root, bot: $0, replies: thread) } ?? .done
        let focusInside = model.focusEntry.map { focus in
            focus.chat == botId && thread.contains { $0.id == focus.entry || ThreadFeed.stableId($0) == focus.entry }
        } == true
        let open = Tasks.isOpen(status, choice: model.taskChoices[root.id])
        let summary = root.data.thread
        let head = Tasks.head(status, thread: summary, mac: model.hostName)
        let fold = Tasks.fold(thread) { !ThreadFeed.isDelivery($0) }
        let line = Tasks.foldLine(messages: fold.messages, steps: fold.steps)
        let waiting = thread.filter { $0.kind == "permission" && $0.data.status == "pending" }
        let asker = waiting.first.flatMap { $0.data.author.flatMap { model.bots[$0] } }
        let tint = AgentTint.resolve(bot?.avatarColor)
        let needsYou = status == .needsYou
        let working = status == .working && summary?.isBusyInTerminal != true
        let guest = bot?.botGuest != nil || bot?.isReadOnly == true
        let destination = model.destination(botId)
        let target = destination.map { !$0.newTask && $0.going?.id == root.id } == true
        let missing = missingReplies(root)
        let toggle = {
            withAnimation(Motion.reduced(Motion.layout, reduceMotion)) { model.taskChoices[root.id] = !open }
        }
        return TaskCardFrame(open: open, live: status == .working, target: target,
                             fill: needsYou ? AgentTint.resolve(asker?.avatarColor ?? bot?.avatarColor) : nil) {
            TaskHeadRow(head: head, status: status, time: root.date, open: open, tint: tint,
                        busyInTerminal: summary?.isBusyInTerminal == true, toggle: toggle,
                        stop: !open && working ? { model.stop(botId, thread: root.id) } : nil,
                        stopping: model.isStopping(botId, thread: root.id))
            entryRow(root, groupStart: true)
            if open {
                ForEach(ThreadFeed.replies(thread)) { reply in
                    switch reply {
                    case .entry(let e, let groupStart):
                        entryRow(e, groupStart: groupStart)
                    case .steps(let run):
                        // Inside an open card the steps show one by one; a tap folds them.
                        StepsRow(run: run, expanded: !expandedSteps.contains(run.id),
                                 toggle: { toggleSteps(run.id) }, openTrace: { showTrace = true })
                    case .separator(let date, _):
                        TimeSeparator(date: date)
                    }
                }
            } else {
                if let line {
                    TaskFoldLine(text: line, toggle: toggle)
                }
                if let last = fold.last { entryRow(last, groupStart: true) }
                ForEach(waiting) { entryRow($0, groupStart: true) }
            }
            if open, working, let bot {
                TaskLiveLine(bot: bot, thinking: model.currentThinking(botId, thread: root.id), stopping: model.isStopping(botId, thread: root.id)) {
                    model.stop(botId, thread: root.id)
                }
            }
            if head.held, summary?.isHeldByTerminal == true, !guest {
                ThreadWriterDock(botId: botId, rootId: root.id, inCard: true) {
                    model.taskTargets[botId] = .task(root.id)
                }
            }
            if !guest, !working, !head.held {
                TaskReplyButton {
                    model.taskTargets[botId] = .task(root.id)
                }
            }
        }
        // A search jump opens the card once; the chevron can fold it again.
        .onChange(of: focusInside, initial: true) { _, inside in if inside { model.taskChoices[root.id] = true } }
        .onAppear { visibleTasks.ids.insert(root.id) }
        .onDisappear { visibleTasks.ids.remove(root.id) }
        .task(id: missing) {
            // The thread's older replies are not here yet: bring them in, so the card has them.
            if missing { await model.loadThread(botId, root: root.id) }
        }
    }
}

/// The card's shape: a spine down the left while open (ink while it is the box's destination or working), and a
/// fill only while it needs you.
private struct TaskCardFrame<Content: View>: View {
    let open: Bool
    let live: Bool
    let target: Bool
    let fill: AgentTint?
    @ViewBuilder let content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 0) { content }
            .padding(.leading, open || target ? 12 : 0)
            .padding(.horizontal, fill != nil ? 12 : 0)
            .padding(.vertical, fill != nil ? 10 : 0)
            .overlay(alignment: .leading) {
                if open || target {
                    RoundedRectangle(cornerRadius: 1)
                        .fill(target || live ? Palette.ink : Palette.line)
                        .frame(width: 2)
                        .padding(.top, 34)
                        .padding(.leading, fill != nil ? 12 : 0)
                        .padding(.bottom, fill != nil ? 10 : 0)
                }
            }
            .background {
                if let fill {
                    RoundedRectangle(cornerRadius: KithRadius.card, style: .continuous)
                        .fill(LinearGradient(colors: [fill.a.opacity(0.14), fill.b.opacity(0.14)], startPoint: .topLeading, endPoint: .bottomTrailing))
                        .overlay(RoundedRectangle(cornerRadius: KithRadius.card, style: .continuous).strokeBorder(fill.a.opacity(0.25), lineWidth: 1))
                }
            }
            .padding(.top, 14)
            .padding(.bottom, 4)
    }
}

/// The card's head line: status, origin (or the terminal holding it), time and the chevron; a tap folds or opens it.
/// Folded while it works, the live line is hidden, so Stop moves up here.
private struct TaskHeadRow: View {
    let head: TaskHead
    let status: TaskStatus
    let time: Date
    let open: Bool
    let tint: AgentTint
    let busyInTerminal: Bool
    let toggle: () -> Void
    let stop: (() -> Void)?
    var stopping = false

    var body: some View {
        HStack(spacing: 8) {
            Button(action: toggle) {
                HStack(spacing: 6) {
                    if head.held, let origin = head.origin {
                        HStack(spacing: 4) {
                            if busyInTerminal { LiveDot(tint: tint) } else { Image(systemName: "terminal").font(.system(size: 10, weight: .medium)) }
                            Text(origin).lineLimit(1)
                        }
                        .foregroundStyle(Palette.ink)
                        .padding(.horizontal, 8)
                        .padding(.vertical, 3)
                        .background(Palette.panel, in: Capsule())
                    }
                    if let word = head.status {
                        HStack(spacing: 4) {
                            if status == .working {
                                LiveDot(tint: tint)
                            } else if status == .needsYou {
                                Circle().fill(Palette.attention).frame(width: 6, height: 6)
                            }
                            Text(word).fontWeight(.semibold).foregroundStyle(status == .needsYou ? Palette.attention : Palette.ink)
                        }
                    }
                    if !head.held, let origin = head.origin {
                        if head.status != nil { Text("·") }
                        Text(origin)
                    }
                    if head.status != nil || head.origin != nil { Text("·") }
                    Text(Self.clock(time))
                    Spacer(minLength: 8)
                    Image(systemName: "chevron.down")
                        .font(.system(size: 10, weight: .semibold))
                        .rotationEffect(.degrees(open ? 180 : 0))
                }
                .kithText(.secondary)
                .foregroundStyle(Palette.muted)
                .frame(minHeight: 28)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityElement(children: .combine)
            .accessibilityValue(open ? "Expanded" : "Collapsed")
            .accessibilityHint(open ? "Folds the task" : "Shows the whole task")
            if let stop { TaskStopButton(action: stop, disabled: stopping) }
        }
    }

    /// 10:31 today; Yesterday 21:14 and Sep 16 9:02 before.
    static func clock(_ date: Date) -> String {
        Calendar.current.isDateInToday(date) ? date.formatted(date: .omitted, time: .shortened) : RelativeTime.separator(date)
    }
}

/// "4 messages · 9 steps", centred between hairlines: what the fold hides. A tap opens the card.
private struct TaskFoldLine: View {
    let text: String
    let toggle: () -> Void

    var body: some View {
        Button(action: toggle) {
            HStack(spacing: 8) {
                Rectangle().fill(Palette.line).frame(height: 1)
                HStack(spacing: 4) {
                    Text(text).kithText(.secondary).fixedSize()
                    Image(systemName: "chevron.down").font(.system(size: 9, weight: .semibold))
                }
                .foregroundStyle(Palette.muted)
                Rectangle().fill(Palette.line).frame(height: 1)
            }
            .frame(minHeight: 32)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .padding(.top, 6)
        .accessibilityLabel(text)
        .accessibilityHint("Shows the whole task")
    }
}

/// The open card's live line: what the agent is doing, and Stop.
private struct TaskLiveLine: View {
    let bot: Bot
    let thinking: String?
    var stopping = false
    let stop: () -> Void

    var body: some View {
        HStack(alignment: .center, spacing: 8) {
            WorkingIndicator(bot: bot, thinking: thinking)
            TaskStopButton(action: stop, disabled: stopping)
        }
        .padding(.top, 6)
    }
}

private struct TaskStopButton: View {
    let action: () -> Void
    var disabled = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: 4) {
                Image(systemName: "stop.fill").font(.system(size: 9))
                Text("Stop").kithText(.secondary).fontWeight(.semibold)
            }
            .foregroundStyle(Palette.ink)
            .padding(.horizontal, 10)
            .frame(minHeight: 30)
            .overlay(Capsule().strokeBorder(Palette.line, lineWidth: 1))
            .contentShape(Capsule())
        }
        .buttonStyle(PressScale())
        .disabled(disabled)
        .opacity(disabled ? 0.5 : 1)
        .accessibilityLabel("Stop this task")
    }
}

/// Under a settled card: points the chat's box at this task.
private struct TaskReplyButton: View {
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            HStack(spacing: 4) {
                Image(systemName: "arrowshape.turn.up.left").font(.system(size: 10))
                Text("Reply").kithText(.secondary)
            }
            .foregroundStyle(Palette.muted)
            .frame(minHeight: 32)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .padding(.top, 2)
        .accessibilityLabel("Reply in this task")
    }
}

/// Over the box: a working task whose card is off screen, with what it is doing. A tap jumps to the card.
struct WorkingNowLine: View {
    let task: Entry
    let more: Int
    let bot: Bot?
    let jump: () -> Void

    var body: some View {
        Button(action: jump) {
            HStack(spacing: 6) {
                LiveDot(tint: AgentTint.resolve(bot?.avatarColor))
                Text(Tasks.title(task)).fontWeight(.semibold).foregroundStyle(Palette.ink).lineLimit(1)
                if let activity = bot?.activity, !activity.isEmpty {
                    Text("· \(activity)").foregroundStyle(Palette.muted).lineLimit(1)
                }
                Spacer(minLength: 4)
                if more > 0 { Text("+\(more)").foregroundStyle(Palette.muted) }
                Image(systemName: "arrow.up").font(.system(size: 10, weight: .semibold)).foregroundStyle(Palette.muted)
            }
            .kithText(.secondary)
            .padding(.horizontal, 12)
            .frame(minHeight: 34)
            .background(Palette.panel, in: Capsule())
            .contentShape(Capsule())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Working now: \(Tasks.title(task))\(more > 0 ? ", and \(more) more" : "")")
        .accessibilityHint("Shows the task")
    }
}
