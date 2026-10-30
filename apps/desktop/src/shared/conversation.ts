// Room conversations (agents talking in a group): what one person message to a group set going, kept per lane so the
// app can draw the live line, the end line and the late label. Everything here tolerates absent fields: a host from
// before leaves out `conversation`, `lateWhy` and `endedWhy`, and old runs ended without a reason.
import { decodeRoomConversationEnd, decodeRoomConversationState, decodeRoomSpeakerKind, type RoomConversationEnd, type RoomConversationState, type RoomSpeakerKind } from './host-contract.gen.ts'

export const DEFAULT_TURNS = 10
export const MIN_TURNS = 1
export const MAX_TURNS = 100
/** The quick picks in the turns popover. */
export const TURN_PICKS = [1, 5, 10, 25, 100] as const

/** A turn count the host accepts: a whole number from 1 to 100; anything unreadable is the default. */
export function clampTurns(n: unknown): number {
  const v = typeof n === 'number' ? n : typeof n === 'string' && n.trim() ? Number(n) : NaN
  if (!Number.isFinite(v)) return DEFAULT_TURNS
  return Math.min(MAX_TURNS, Math.max(MIN_TURNS, Math.round(v)))
}

export interface Speaker {
  id: string
  kind: RoomSpeakerKind
  name: string
}

export interface Conversation {
  id: string
  chat: string
  /** A thread's id; null for the room's main lane. */
  lane: string | null
  budget: number
  left: number
  state: RoomConversationState
  /** null while open, and for a run that ended before hosts kept the reason; a word this app doesn't know is 'unknown'. */
  endedWhy: RoomConversationEnd | 'unknown' | null
  current: Speaker | null
  /** Unix ms when an outside bot's turn times out. */
  waitUntil: number | null
  /** The entry of the person message that opened it. */
  opening: string | null
}

const rec = (v: unknown): Record<string, unknown> => (v && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : {})
const str = (v: unknown): string => (typeof v === 'string' ? v : '')
const int = (v: unknown): number | null => (typeof v === 'number' && Number.isFinite(v) ? v : null)

/** A conversation object (an event's fields or the `conversation` of a history answer); null when it has no id or room. */
export function parseConversation(raw: unknown): Conversation | null {
  const r = rec(raw)
  const id = str(r.conversation)
  const chat = str(r.chat)
  if (!id || !chat) return null
  const budget = Math.max(0, int(r.budget) ?? 0)
  const speaker = rec(r.current)
  const state = decodeRoomConversationState(r.state)
  const why = typeof r.endedWhy === 'string' ? decodeRoomConversationEnd(r.endedWhy) : null
  return {
    id,
    chat,
    lane: str(r.lane) || null,
    budget,
    left: Math.min(budget, Math.max(0, int(r.left) ?? 0)),
    state,
    endedWhy: state === 'open' ? null : why,
    current: str(speaker.id) ? { id: str(speaker.id), kind: decodeRoomSpeakerKind(speaker.kind), name: str(speaker.name) || 'Someone' } : null,
    waitUntil: int(r.waitUntil),
    opening: str(r.opening) || null,
  }
}

export const isOpen = (c: Conversation) => c.state === 'open'

/** How far a conversation has got: it only moves forward (open with fewer turns left, then ended, then ended with its reason). */
function rank(c: Conversation): number {
  if (c.state === 'open') return (c.budget - c.left) * 4
  return (c.budget + 1) * 4 + (c.endedWhy ? 1 : 0)
}

export const laneKey = (chat: string, lane: string | null) => `${chat}\u0000${lane ?? ''}`

/**
 * The conversations an owner's app has heard of. Events can arrive out of order or twice: for one conversation the
 * newest state wins; for one lane a conversation that opens takes it, and an old run's late end never closes a
 * newer one that is still open.
 */
export class ConversationBook {
  private byId = new Map<string, Conversation>()
  private latest = new Map<string, string>()
  /** Bumps on every change, for memo dependencies. */
  version = 0

  /** Takes a report in; false when it changed nothing. */
  apply(next: Conversation | null | undefined): boolean {
    if (!next) return false
    const known = this.byId.get(next.id)
    if (known) {
      const [a, b] = [rank(next), rank(known)]
      if (a < b) return false
      // The same stage again changes only who is up and how long the room waits.
      if (a === b && next.waitUntil === known.waitUntil && next.current?.id === known.current?.id) return false
    }
    this.byId.set(next.id, next)
    const key = laneKey(next.chat, next.lane)
    const head = this.get(this.latest.get(key))
    const takesLane = !head || head.id === next.id || isOpen(next) || !isOpen(head)
    if (takesLane) this.latest.set(key, next.id)
    this.version++
    return true
  }

  get(id: string | null | undefined): Conversation | null {
    return (id && this.byId.get(id)) || null
  }

  /** The lane's newest conversation. */
  inLane(chat: string, lane: string | null): Conversation | null {
    return this.get(this.latest.get(laneKey(chat, lane)))
  }

  /** The lanes' newest conversations of a chat that ended with a line to show. */
  endedIn(chat: string): Conversation[] {
    const out: Conversation[] = []
    for (const [key, id] of this.latest) {
      if (!key.startsWith(`${chat}\u0000`)) continue
      const c = this.byId.get(id)
      if (c && endLine(c)) out.push(c)
    }
    return out
  }

  clear() {
    this.byId.clear()
    this.latest.clear()
    this.version++
  }
}

const turns = (n: number) => (n === 1 ? 'turn' : 'turns')
const used = (c: Conversation) => Math.max(0, c.budget - c.left)

/** The line where a run ends, or null when it needs none (still open, idle, a newer message, an old run without a reason). */
export function endLine(c: Conversation): { text: string; more: boolean } | null {
  if (isOpen(c)) return null
  const why = c.endedWhy ?? (c.state === 'stopped' ? 'stopped' : null)
  switch (why) {
    case 'spent':
      return { text: `Used all ${c.budget} ${turns(c.budget)}`, more: true }
    case 'quiet':
      return { text: `Worked it out · ${used(c)} of ${c.budget} ${turns(c.budget)}`, more: false }
    case 'stopped':
      return { text: `You stopped it · ${used(c)} of ${c.budget} ${turns(c.budget)} used`, more: false }
    default:
      return null
  }
}

/** The live line above the box: whose turn, the turns left, and how long a bot's turn waits. */
export function liveLine(c: Conversation, now: number): { who: string; botTurn: boolean; wait: string | null; left: string; fraction: number } {
  const bot = c.current?.kind === 'bot'
  const ms = c.waitUntil !== null ? c.waitUntil - now : null
  return {
    who: c.current?.name ?? '',
    botTurn: !!bot,
    wait: bot && ms !== null ? waitText(ms) : null,
    left: `${c.left} of ${c.budget} ${turns(c.budget)} left`,
    fraction: c.budget > 0 ? c.left / c.budget : 0,
  }
}

/** "waits up to 5 min": whole minutes, 1 at the least. */
export function waitText(ms: number): string {
  return `waits up to ${Math.max(1, Math.ceil(ms / 60_000))} min`
}

/** Why a late reply starts nothing; null for a reason this app doesn't know or a host that gave none. */
export function lateReason(why: unknown): string | null {
  switch (why) {
    case 'stopped': return 'Came after you stopped, so it starts nothing.'
    case 'newer': return 'Came after your newer message, so it starts nothing.'
    case 'ended': return 'Came after the run ended, so it starts nothing.'
    case 'noTurn': return "It wasn't its turn, so it starts nothing."
    default: return null
  }
}

/** A reply's lateness from its entry data: null when it isn't late. An unprompted post carries no `late`. */
export function lateOf(data: { late?: unknown; lateWhy?: unknown }): { reason: string | null } | null {
  if (data.late !== true) return null
  return { reason: lateReason(data.lateWhy) }
}
