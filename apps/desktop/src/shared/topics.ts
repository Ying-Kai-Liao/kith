import type { Entry } from './models.ts'

/** A topic as the host stores it on its root entry (`entry.data.topic`): a run of the room folded into one card. */
export interface Topic {
  id: string
  title: string
  untilId: string
  untilAt: number
  turns: number[]
  createdAt: number
}

/** The shape the fold rule reads. */
type Stamped = Pick<Entry, 'createdAt' | 'turn'>

/** The most a topic title can be (the host refuses more). */
export const TOPIC_TITLE_MAX = 80

/**
 * Whether `e` belongs to the topic whose first message is `root`: it came between the root and `untilAt` (same
 * millisecond counts), or it belongs to a turn the topic holds and came after the root. Same rule as the host's.
 */
export function inTopic(root: Stamped, topic: Topic, e: Stamped): boolean {
  if (e.createdAt < root.createdAt) return false
  return e.createdAt <= topic.untilAt || topic.turns.includes(e.turn)
}

/** The topic an entry roots, if any. */
export const topicOf = (e: Entry): Topic | null => e.data.topic ?? null

export interface Folds {
  /** A root's id to everything it folds, root first, in the order given. */
  folds: Map<string, Entry[]>
  /** Every entry that is folded into a topic other than as its root. */
  hidden: Set<Entry>
}

/**
 * Finds the topics of a chat's flat view. A root whose own message isn't loaded can't be here, so nothing folds then.
 * A topic never sits inside another: a root already folded into an earlier topic is an ordinary message.
 */
export function foldTopics(flat: Entry[]): Folds {
  const folds = new Map<string, Entry[]>()
  const hidden = new Set<Entry>()
  for (const root of flat) {
    const topic = topicOf(root)
    if (!topic || hidden.has(root)) continue
    const members = flat.filter((e) => e === root || (!hidden.has(e) && !folds.has(e.id) && inTopic(root, topic, e)))
    folds.set(root.id, members)
    for (const e of members) if (e !== root) hidden.add(e)
  }
  return { folds, hidden }
}
