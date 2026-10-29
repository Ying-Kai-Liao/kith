// Tasks (host `chat/tasks.rs`): each burst of the user's chat with a bot is a task. Its first message is a root in
// the chat (`data.task`); the bot's answers and the rest of the burst are its thread. The chat is flat: every
// reply shows in it in time order, and the host routes a message to the current task.
import { isWorking, isWorkingIn, type Bot, type Entry } from './models.ts'
import { isBusyInTerminal, isHeldElsewhere } from './thread-writer.ts'

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
