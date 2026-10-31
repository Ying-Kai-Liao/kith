// A card drawn over an entry, `data.richCard`: the host makes it from a tool result or a run's outbox item, and the
// entry's `text` stays its twin (Telegram, Discord, Slack, notifications, and any app that can't draw it). The
// permission entries' `data.card` is the other kind, the Access and Key cards (secret-card.ts).

import type { EntryData } from './models.ts'

export interface RichCard {
  /** schedule · routine · news · result; open string. */
  type: string
  /** The version of that type's shape; an app draws only the versions it knows. */
  v: number
  /** The live record (a routine's id, an agent and its job's id): the app shows its current state. */
  ref?: Record<string, unknown>
  /** The record as it was when the card was made, for when it is gone or the host can't say. */
  snapshot?: Record<string, unknown>
}

/** The card types this app draws, each with the newest version it knows. */
export const KNOWN_CARDS: Readonly<Record<string, number>> = { schedule: 1, news: 1, result: 2, plan: 1 }

const isObject = (v: unknown): v is Record<string, unknown> => !!v && typeof v === 'object' && !Array.isArray(v)

/**
 * The entry's card when this app can draw it. Null, so the entry is the plain row it always was, for no card, a type
 * this app doesn't know, a newer version than it knows, or anything that isn't a card.
 */
export function richCardOf(data: EntryData, known: Readonly<Record<string, number>> = KNOWN_CARDS): RichCard | null {
  const card: unknown = data.richCard
  if (!isObject(card)) return null
  const { type, v, ref, snapshot } = card
  if (typeof type !== 'string' || !Number.isInteger(v) || (v as number) < 1) return null
  if ((ref !== undefined && !isObject(ref)) || (snapshot !== undefined && !isObject(snapshot))) return null
  return Object.hasOwn(known, type) && (v as number) <= known[type] ? (card as unknown as RichCard) : null
}
