// An approved permission card whose session can't resume because an old run still holds its writer claim (host
// `cards/resume.rs`; designer's spec app-style/held-run). `entry.data.held`; the host clears it when the run ends, is
// stopped, or the card expires. Port of apps/ios/Kit/Sources/KithKit/Models/HeldRun.swift; the wording must stay identical.
import type { Entry, HeldRun } from './models.ts'

/** What `stopHolder` answers: who was ended, or null when nothing live held the session any more. `ownTurn` is the host's own turn. */
export interface StopHolderResult {
  stopped: { kind: string; pid: number; since: string } | null
  ownTurn?: { kind: string; pid: number; since: string } | null
  /** Signalled, but something of the run is still in the process table: its claim is kept. Stop again. */
  stillEnding?: { kind: string; pid: number; since: string } | null
  /** This computer can't stop a run (no process signals): nothing was done. */
  cantStop?: { kind: string; pid: number; since: string } | null
}

export type HeldPhase = { kind: 'idle' } | { kind: 'stopping' } | { kind: 'failed'; reason: string; retry: boolean }

export const IDLE: HeldPhase = { kind: 'idle' }

/** An answered permission card an old run still holds up. It is not a card that waits on the reader. */
export const isHeldPermission = (e: Pick<Entry, 'kind' | 'data'>) => e.kind === 'permission' && e.data.status === 'answered' && !!e.data.held

const NOUNS: Record<string, string> = { oneshot: 'background run', bot: 'chat run', app: 'app turn' }
export const heldNoun = (kind: string) => NOUNS[kind] ?? 'terminal run'

const ICONS: Record<string, string> = { oneshot: 'gearshape.2', bot: 'bubble.left.and.bubble.right', app: 'macwindow' }
export const heldIcon = (kind: string) => ICONS[kind] ?? 'terminal'

const clock = (d: Date) => `${d.getHours()}:${String(d.getMinutes()).padStart(2, '0')}`
const DAY = 86_400_000

/** `9:40`, `yesterday 21:10` past midnight, a weekday inside a week, a date past it. */
export function sinceText(since: string, now: Date = new Date()): string {
  const at = new Date(since)
  if (Number.isNaN(at.getTime())) return ''
  return clockText(at, now)
}

function clockText(at: Date, now: Date): string {
  const midnight = (d: Date) => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime()
  const days = Math.round((midnight(now) - midnight(at)) / DAY)
  if (days === 0) return clock(at)
  if (days === 1) return `yesterday ${clock(at)}`
  if (now.getTime() - at.getTime() < 7 * DAY) return `${at.toLocaleDateString('en-US', { weekday: 'short' })} ${clock(at)}`
  return at.toLocaleDateString('en-US', { month: 'short', day: 'numeric' })
}

/** The transcript divider after a Stop: "Stopped the old terminal run · 10:02". */
export const stoppedDivider = (kind: string, at: Date) => `Stopped the old ${heldNoun(kind)} · ${clock(at)}`

export interface HeldRowModel {
  icon: string
  title: string
  sentence: string
  /** The desktop tooltip on the row; the pid shows only here and in the confirm. */
  tooltip: string
  /** The button's label, or null when this person (or this failure) has none. */
  button: 'Stop' | 'Try again' | null
  stopping: boolean
  failed: boolean
}

/** Only the Mac's owner sees Stop; members and bot guests read who can. */
export function heldRowOf({ held, owner, ownerName, phase = IDLE, now = new Date() }: { held: HeldRun; owner: boolean; ownerName?: string | null; phase?: HeldPhase; now?: Date }): HeldRowModel {
  const since = sinceText(held.since, now)
  const base = `Since ${since}. It goes on when that ends`
  const idle = owner ? `${base}.` : `${base}, or when ${ownerName || 'the owner'} stops it.`
  const sentence = phase.kind === 'stopping' ? 'Stopping it…' : phase.kind === 'failed' ? `Couldn't stop it. ${phase.reason}` : idle
  const canStop = owner && !(phase.kind === 'failed' && !phase.retry)
  return {
    icon: heldIcon(held.kind),
    title: `An old ${heldNoun(held.kind)} still has this session`,
    sentence,
    tooltip: `${held.kind} · pid ${held.pid} · since ${since}`,
    button: !canStop ? null : phase.kind === 'failed' ? 'Try again' : 'Stop',
    stopping: phase.kind === 'stopping',
    failed: phase.kind === 'failed',
  }
}

export const confirmTitle = 'Stop the old run?'

export function confirmMessage({ held, mac, agent, now = new Date() }: { held: HeldRun; mac: string; agent: string; now?: Date }): string {
  const what = held.kind === 'app' ? 'An app turn' : held.kind === 'bot' ? 'A chat run' : held.kind === 'oneshot' ? 'A background run' : 'A terminal session'
  return `${what} on ${mac} has had this session since ${sinceText(held.since, now)}. Kith ends it (pid ${held.pid}) so ${agent} can go on with what you allowed. Anything it's in the middle of stops too.`
}

export const OWN_TURN: HeldPhase = { kind: 'failed', reason: "That's Kith's own turn. It ends by itself.", retry: false }
export const STILL_ENDING: HeldPhase = { kind: 'failed', reason: 'Still ending — press Stop again', retry: true }
export const CANT_STOP: HeldPhase = { kind: 'failed', reason: "This computer can't stop that run", retry: false }

/** How a failed `stopHolder` reads. A 403 means this account isn't the owner (the button should not have shown). */
export function failureOf(error: unknown, mac: string): HeldPhase {
  if ((error as { status?: number } | null)?.status === 403) return { kind: 'failed', reason: 'Only the owner can stop it.', retry: false }
  return { kind: 'failed', reason: `Try again, or close it on ${mac}.`, retry: true }
}

/** The thread row's subtitle: the status word, or the old run an approved card waits behind (no needs-you). */
export const taskLabel = (statusLabel: string, replies: Pick<Entry, 'kind' | 'data'>[]) => (replies.some(isHeldPermission) ? 'Waiting for an old run' : statusLabel)
