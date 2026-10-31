// The owner's peek at a handoff (a `kith ask`): host `delegationPeek` / `delegationsPeek` (serve/host/src/api/delegations.rs).
// Owner only; the apps read it as received, so a state or field this app doesn't know is still drawn.

export type PeekState = 'sent' | 'working' | 'needsYou' | 'done' | 'stopped' | 'unknown'

const STATES: readonly PeekState[] = ['sent', 'working', 'needsYou', 'done', 'stopped', 'unknown']

export interface PeekLimits {
  costMicroUsd: number | null
  capMicroUsd: number | null
  chainCostMicroUsd: number | null
  hop: number | null
  hopMax: number
  crossHops: number | null
  crossMax: number
  elapsedMs: number | null
}

export interface Peek {
  id: string
  state: PeekState
  brief: string | null
  from: { agent: string | null; person: string | null }
  to: string
  crossed: boolean
  /** The worker's thread on this Mac; a null `threadId` is the bot's main chat. Null when this Mac doesn't know it. */
  thread: { botId: string; threadId: string | null } | null
  /** The newest tool row or text of the worker, frozen once it is done. */
  live: { kind: 'tool' | 'text'; text: string } | null
  limits: PeekLimits
  startedMs: number
  answeredMs: number | null
  cappedMs: number | null
}

const num = (v: unknown): number | null => (typeof v === 'number' && Number.isFinite(v) ? v : null)
const str = (v: unknown): string | null => (typeof v === 'string' && v ? v : null)
const rec = (v: unknown): Record<string, unknown> => (v && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : {})

/** One peek as the host sent it; null when it has no id. A state this app doesn't know reads as `unknown`. */
export function parsePeek(raw: unknown): Peek | null {
  const p = rec(raw)
  const id = str(p.id)
  if (!id) return null
  const state = (STATES as readonly unknown[]).includes(p.state) ? (p.state as PeekState) : 'unknown'
  const from = rec(p.from)
  const t = rec(p.thread)
  const bot = str(t.botId)
  const live = rec(p.live)
  const text = str(live.text)
  const l = rec(p.limits)
  return {
    id,
    state,
    brief: str(p.brief),
    from: { agent: str(from.agent), person: str(from.person) },
    to: str(p.to) ?? '',
    crossed: p.crossed === true,
    thread: bot ? { botId: bot, threadId: str(t.threadId) } : null,
    live: text ? { kind: live.kind === 'tool' ? 'tool' : 'text', text } : null,
    limits: {
      costMicroUsd: num(l.costMicroUsd),
      capMicroUsd: num(l.capMicroUsd),
      chainCostMicroUsd: num(l.chainCostMicroUsd),
      hop: num(l.hop),
      hopMax: num(l.hopMax) ?? 8,
      crossHops: num(l.crossHops),
      crossMax: num(l.crossMax) ?? 2,
      elapsedMs: num(l.elapsedMs),
    },
    startedMs: num(p.startedMs) ?? 0,
    answeredMs: num(p.answeredMs),
    cappedMs: num(p.cappedMs),
  }
}

/** The ones still going or waiting on the owner, from `delegationsPeek`. */
export const parsePeeks = (raw: unknown): Peek[] => {
  const list = rec(raw).peeks
  return Array.isArray(list) ? list.flatMap((p) => parsePeek(p) ?? []) : []
}

const ASK_ID = /\b(?:bg|ask)-\d{14}-[0-9a-f]{6}\b/

/**
 * The ask a tool row belongs to. `kith ask --bg` prints "started echo in the background as bg-…", a run waiting on a
 * card prints its `id`, and `--wait` / `--to` name it on the command line. A plain foreground ask prints no id until it
 * is done, so its row is not tappable; neither is a row from before the delegation record.
 */
export function askIdOf(command?: string, output?: string): string | null {
  if (!command || !/\bkith(?:\s+|-)ask\b/.test(command)) return null
  return ASK_ID.exec(command)?.[0] ?? ASK_ID.exec(output ?? '')?.[0] ?? null
}

export const isOpen = (s: PeekState) => s === 'sent' || s === 'working' || s === 'needsYou'

const dollars = (micro: number) => {
  const d = micro / 1_000_000
  return `$${Number.isInteger(d) ? d : d.toFixed(2)}`
}

export interface LimitPart {
  kind: 'budget' | 'hops' | 'crossings' | 'elapsed'
  label: string
  /** 0..1 of the limit used; none for a clock, which has no limit. */
  fraction: number | null
  /** The one closest to its limit. */
  watch: boolean
}

export function elapsedLabel(ms: number): string {
  const s = Math.floor(ms / 1000)
  if (s < 60) return `${s} s`
  const m = Math.floor(s / 60)
  return m < 60 ? `${m} min` : `${Math.floor(m / 60)} h ${m % 60} min`
}

/** The limits line: budget bar and "$0.70 / $3", hops, crossings, elapsed; whichever is closest to its limit is flagged. */
export function limitParts(l: PeekLimits): LimitPart[] {
  const parts: LimitPart[] = []
  const spent = l.chainCostMicroUsd ?? l.costMicroUsd
  if (spent !== null && l.capMicroUsd) {
    parts.push({ kind: 'budget', label: `${dollars(spent)} / ${dollars(l.capMicroUsd)}`, fraction: Math.min(1, spent / l.capMicroUsd), watch: false })
  } else if (spent !== null) {
    parts.push({ kind: 'budget', label: dollars(spent), fraction: null, watch: false })
  }
  if (l.hop !== null) parts.push({ kind: 'hops', label: `hop ${l.hop} / ${l.hopMax}`, fraction: Math.min(1, l.hop / l.hopMax), watch: false })
  if (l.crossHops) parts.push({ kind: 'crossings', label: `${l.crossHops} / ${l.crossMax} crossings`, fraction: Math.min(1, l.crossHops / l.crossMax), watch: false })
  if (l.elapsedMs !== null) parts.push({ kind: 'elapsed', label: elapsedLabel(l.elapsedMs), fraction: null, watch: false })
  let best = -1
  parts.forEach((p, i) => {
    if (p.fraction !== null && p.fraction > (parts[best]?.fraction ?? 0)) best = i
  })
  if (best >= 0) parts[best] = { ...parts[best]!, watch: true }
  return parts
}

export const stateLabel = (s: PeekState) =>
  ({ sent: 'Sent', working: 'Working', needsYou: 'Needs you', done: 'Done', stopped: 'You stopped it', unknown: 'No record' })[s]

/** Why Join is off, or null when it is on. */
export function joinBlocked(p: Peek): string | null {
  if (p.thread) return null
  return p.crossed ? 'It runs on another Mac' : p.state === 'sent' ? 'It has not started yet' : "Its thread isn't on this Mac"
}

/** Why Stop is off, or null when it is on. A finished or stopped ask has nothing to stop. */
export function stopBlocked(p: Peek): string | null {
  if (!isOpen(p.state)) return p.state === 'unknown' ? 'No record of it' : 'It is not running'
  return p.thread ? null : joinBlocked(p)
}

/** The box to the worker writes into its thread: open work with a thread on this Mac. */
export const sendBlocked = (p: Peek): string | null => (isOpen(p.state) ? joinBlocked(p) : 'It is not running')
