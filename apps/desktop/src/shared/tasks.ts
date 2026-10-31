// Tasks (host `chat/tasks.rs`): each burst of the user's chat with a bot is a task. Its first message is a root in
// the chat (`data.task`); the bot's answers and the rest of the burst are its thread. The chat is flat: every
// reply shows in it in time order, and the host routes a message to the current task.
import { isChat, isWorking, isWorkingIn, type Bot, type Entry, type ThreadSummary } from './models.ts'
import { isBusyInTerminal, isHeldByTerminal, isHeldElsewhere } from './thread-writer.ts'

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

/**
 * Where a chat's turn runs: its thread (null for the chat itself), or undefined when nothing runs in it. The flat
 * chat shows one working indicator whichever thread the work is in.
 */
export function workingThreadIn(b: Bot, chat: string): string | null | undefined {
  if (b.workingLanes) {
    const lane = b.workingLanes.find((l) => l.chat === chat)
    return lane ? (lane.thread ?? null) : undefined
  }
  return isWorking(b) && (b.workingChat ?? b.id) === chat ? (b.workingThread ?? null) : undefined
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

/** A card's title: the task's first message, on one line. */
export const taskTitle = (root: Entry) => (root.data.text ?? '').replace(/\s+/g, ' ').trim() || 'Task'

/** The host's `tasks::IDLE_MS`: a task idle this long is over, and the next message starts a new one. */
export const TASK_IDLE_MS = 10 * 60_000

/**
 * Whether a task card shows its whole thread: while it works; it folds when it stops. A task that needs you stays
 * folded with its question on the card. A choice the user made on this device wins.
 */
export const taskOpen = (status: TaskStatus, choice?: boolean) => choice ?? status === 'working'

/**
 * What a folded card shows and hides: the task's last reply under its first message, and how many messages and steps
 * sit between them (only what the fold hides; a delivery tool is no step). `last` is null before the bot has answered.
 */
export function taskFold(thread: Entry[], isStep: (e: Entry) => boolean = () => true): { last: Entry | null; messages: number; steps: number } {
  let last: Entry | null = null
  for (let i = thread.length - 1; i >= 0 && !last; i--) if (thread[i]!.kind === 'agent' && thread[i]!.data.final === true) last = thread[i]!
  let messages = 0
  let steps = 0
  for (const e of thread) {
    if (e === last) continue
    if (e.kind === 'tool') steps += isStep(e) ? 1 : 0
    else if (isChat(e) && e.kind !== 'permission') messages++
  }
  return { last, messages, steps }
}

/** The fold line: "4 messages · 9 steps"; null when the fold hides nothing. */
export function foldLine(messages: number, steps: number): string | null {
  const parts = [messages ? `${messages} ${messages === 1 ? 'message' : 'messages'}` : '', steps ? `${steps} ${steps === 1 ? 'step' : 'steps'}` : ''].filter(Boolean)
  return parts.length ? parts.join(' · ') : null
}

const ORIGIN_LABEL: Record<string, string> = { terminal: 'Terminal', telegram: 'Telegram', discord: 'Discord', slack: 'Slack', bot: 'Bot' }

/**
 * A card's head line: the status word (none for a done task), and where the task came from when that isn't the app.
 * While another writer holds the thread, who holds it takes the origin's place.
 */
export function taskHead(status: TaskStatus, thread: Entry['data']['thread'], mac: string): { status: string | null; origin: string | null; held: boolean } {
  const word = { working: 'Working', needsYou: 'Needs you', stopped: 'Stopped', failed: 'Failed', done: null }[status]
  if (isHeldElsewhere(thread)) return { status: word, origin: thread?.liveWriter === 'terminal' ? `In terminal · ${mac}` : 'Open elsewhere', held: true }
  const origin = thread?.origin && thread.origin !== 'app' ? (ORIGIN_LABEL[thread.origin] ?? thread.origin) : null
  return { status: word, origin, held: false }
}

/**
 * The task the chat's box goes on with when the user hasn't picked one: the newest task while it works or until it
 * has been idle for ten minutes, as the host routes it (`tasks::continued`). A terminal's task is never picked: it is
 * continued by naming it. Null: the next message starts a new task.
 */
export function continuingTask(bot: Bot, chat: Entry[], now: number): Entry | null {
  for (let i = chat.length - 1; i >= 0; i--) {
    const e = chat[i]!
    if (!isTask(e) || e.data.thread?.origin === 'terminal') continue
    return isWorkingIn(bot, bot.id, e.id) || now - taskLastAt(e) < TASK_IDLE_MS ? e : null
  }
  return null
}

/** The task a message belongs to: its thread's root, or itself when it starts one. */
export const taskOf = (e: Entry): string | null => e.threadId ?? (isTask(e) ? e.id : null)

/**
 * Sends into a task. A task a terminal holds while it is idle is taken over first, and the message goes only once the
 * take-over succeeded: a failed take-over sends nothing (the caller keeps the text in the box). A busy terminal is not
 * taken from; the host queues the message until it lets go.
 */
export async function sendInTask(thread: ThreadSummary | undefined, takeOver: () => Promise<unknown>, send: () => void): Promise<boolean> {
  if (isHeldByTerminal(thread) && !isBusyInTerminal(thread)) {
    try {
      await takeOver()
    } catch {
      return false
    }
  }
  send()
  return true
}
