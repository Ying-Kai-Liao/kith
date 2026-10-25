// Tasks (host `chat/tasks.rs`): each burst of the user's chat with a bot is a task. Its first message is a root in
// the chat (`data.task`); the bot's answers and the rest of the burst are its thread. The chat shows a task inline
// while it's going and folds it into a card once it's idle.
import { isWorkingIn, type Bot, type Entry } from './models.ts'
import { isBusyInTerminal, isHeldElsewhere } from './thread-writer.ts'

/** The host's `tasks::IDLE_MS`: a task idle this long is over, and folds. */
export const TASK_IDLE_MS = 10 * 60_000

export type TaskStatus = 'working' | 'needsYou' | 'stopped' | 'failed' | 'done'

export const isTask = (e: Entry) => e.kind === 'user' && !e.threadId && e.data.task === true

/** The newest task in a chat: where a message without a thread goes on, and what the chat's Stop stops. */
export function currentTask(chat: Entry[]): Entry | null {
  for (let i = chat.length - 1; i >= 0; i--) if (isTask(chat[i]!)) return chat[i]!
  return null
}

/** The chat with its tasks' threads (other threads left out): what a voice call hears and reads. */
export function chatAndTasks(all: Entry[]): Entry[] {
  const tasks = new Set(all.filter(isTask).map((e) => e.id))
  return all.filter((e) => !e.threadId || tasks.has(e.threadId))
}

/** Where the task stands: at work (or waiting on an approval), stopped, failed, or done. */
export function taskStatus(bot: Bot, root: Entry, replies: Entry[]): TaskStatus {
  if (isWorkingIn(bot, bot.id, root.id)) {
    // A prompt in a thread another writer holds is read-only here, so it isn't ours to answer.
    const held = isHeldElsewhere(root.data.thread)
    return !held && replies.some((e) => e.kind === 'permission' && e.data.status === 'pending') ? 'needsYou' : 'working'
  }
  // A session started in a terminal works outside the host's lanes; its thread says so.
  if (isBusyInTerminal(root.data.thread)) return 'working'
  // A message parked at its agent's monthly limit waits under a card with no lane at work.
  if (replies.some((e) => e.kind === 'permission' && e.data.status === 'pending' && e.data.toolKind === 'limit')) return 'needsYou'
  // The host says how a finished turn ended; an older host doesn't, so the thread's entries tell. A host "working"
  // or "needsYou" while no lane is working is stale (the host restarted), so it isn't taken.
  const said = root.data.thread?.status
  if (said === 'stopped' || said === 'failed' || said === 'done') return said
  const last = [...replies].reverse().find((e) => e.kind === 'notice' || (e.kind === 'agent' && e.data.final === true))
  return last?.kind === 'notice' && last.data.text === 'Stopped.' ? 'stopped' : 'done'
}

/** When the task last moved: its newest message, or its start. */
export const taskLastAt = (root: Entry) => (root.data.thread?.lastAt || root.createdAt)

/**
 * Whether a task shows inline rather than as a card: while it works or waits on the user, and the newest one until
 * it has been idle for ten minutes. A choice the user made on this device wins.
 */
export function taskOpen(status: TaskStatus, newest: boolean, lastAt: number, now: number, choice?: boolean): boolean {
  if (choice !== undefined) return choice
  return status === 'working' || status === 'needsYou' || (newest && now - lastAt < TASK_IDLE_MS)
}

/** A card's title: the task's first message, on one line. */
export const taskTitle = (root: Entry) => (root.data.text ?? '').replace(/\s+/g, ' ').trim() || 'Task'

/**
 * The main chat's order with tasks placed by their activity. A task keeps its root's place until it moves after the
 * chat's newest other message (a terminal session started days ago, still talking): then it sits at the end, by its
 * latest reply, where new things appear. `moved` are the ids of those tasks.
 */
export function placeTasks<T extends { entry: Entry }>(slots: T[]): { slots: T[]; moved: Set<string> } {
  let latest = 0
  for (const s of slots) if (!isTask(s.entry)) latest = Math.max(latest, s.entry.createdAt)
  const moved = new Map<string, T>()
  for (const s of slots) if (isTask(s.entry) && taskLastAt(s.entry) > latest) moved.set(s.entry.id, s)
  if (moved.size === 0) return { slots, moved: new Set() }
  const rest = slots.filter((s) => !moved.has(s.entry.id))
  const tail = [...moved.values()].sort((a, b) => taskLastAt(a.entry) - taskLastAt(b.entry))
  return { slots: [...rest, ...tail], moved: new Set(moved.keys()) }
}
