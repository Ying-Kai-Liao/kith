// The Schedule card (DESIGN.md "Schedule"): a Kith agent's kith-sched job (`ref {agent, id}`) or an outside bot's
// routine (`ref {bot, routine}`), drawn from the live job when the app can read it and from the entry's snapshot when it
// can't or the job is gone. Every word is host state or the card's fixed text.

import type { KithSchedule } from './kith-info.ts'
import type { RichCard } from './rich-card.ts'

/** What the app read of the job just now. `null` from the source means it answered and the job isn't there. */
export interface ScheduleLive {
  paused: boolean
  nextRun: number | null
  last: { running: boolean; at: number } | null
  /** A routine's last run failed (Kith jobs carry no success word). */
  failed?: boolean
  error?: string | null
}

export interface ScheduleFold {
  text: string
  /** When, on the right: the entry's time or day. */
  trailing: string
  /** A pill after its day opens in place; a changed or removed card stays a line. */
  opens: boolean
  done: boolean
}

export interface ScheduleView {
  kind: 'job' | 'routine'
  name: string
  /** "Added · 9:12", "Updated · Tue"; null when paused (the capsule shows instead). */
  op: string | null
  paused: boolean
  every: string
  next: string
  last: { text: string; running: boolean; failed: boolean; error: string | null }
  /** Whom a Kith job runs as: "you" or a person's name. Null for a routine (it runs as its bot). */
  runsAs: string | null
  task: string
  fold: ScheduleFold | null
}

const str = (v: unknown): string | null => (typeof v === 'string' && v.trim() ? v : null)
const ms = (v: unknown): number | null => {
  if (typeof v === 'number' && Number.isFinite(v)) return v
  const t = typeof v === 'string' ? Date.parse(v) : NaN
  return Number.isNaN(t) ? null : t
}

const DAYS = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat']
const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec']

export const clock = (at: number) => {
  const d = new Date(at)
  return `${d.getHours()}:${String(d.getMinutes()).padStart(2, '0')}`
}

const dayStart = (at: number) => new Date(at).setHours(0, 0, 0, 0)
/** Whole calendar days from `now` to `at` (negative in the past). */
export const daysFrom = (at: number, now: number) => Math.round((dayStart(at) - dayStart(now)) / 86_400_000)

/** "Today", "Tomorrow", "Yesterday", a weekday within six days, else "31 Oct". */
export function dayWord(at: number, now: number): string {
  const d = new Date(at)
  const days = daysFrom(at, now)
  if (days === 0) return 'Today'
  if (days === 1) return 'Tomorrow'
  if (days === -1) return 'Yesterday'
  if (Math.abs(days) <= 6) return DAYS[d.getDay()]!
  return `${d.getDate()} ${MONTHS[d.getMonth()]}`
}

/** A time on a row: "Today 7:43", "Fri 7:30", "31 Oct 7:43". */
export const whenWords = (at: number, now: number) => `${dayWord(at, now)} ${clock(at)}`

/** When an entry was posted, for a header or a pill: the time on its own day, else the day ("Tue", "31 Oct"). */
export function postedWords(at: number, now: number): string {
  const days = daysFrom(at, now)
  if (days === 0) return clock(at)
  return days === -1 ? 'Yesterday' : dayWord(at, now)
}

/** The schedule in a pill's quiet words: "Every day at 7:43" is "every day 7:43". */
export const quietEvery = (every: string) => (every ? every[0]!.toLowerCase() + every.slice(1) : '').replace(/ at (?=\d)/, ' ')

/** Paths in a task shortened to their last two folders: "…/daily-news/SKILL.md". */
export const shortPaths = (task: string) =>
  task.replace(/(?:~|\/)[^\s'"`]*\/([^\s/'"`]+\/[^\s/'"`]+\/[^\s/'"`]+)/g, (whole, tail: string) => (whole.length > tail.length + 1 ? `…/${tail}` : whole))

const OPS: Record<string, string> = { added: 'Added', updated: 'Updated', resumed: 'Resumed', paused: 'Paused', created: 'Created', removed: 'Removed' }

/**
 * The card as it reads now. `live` is the job as the app read it (undefined: not read, so the snapshot shows; null:
 * read and gone). `viewerIsPerson` is whether the viewer is the person the job runs as; `personName` names anyone else.
 */
export function scheduleView(card: RichCard, opts: {
  postedAt: number
  now: number
  live?: ScheduleLive | null
  viewerIsPerson: boolean
  personName: string | null
  /** The pill after its day was tapped open. */
  opened?: boolean
}): ScheduleView {
  const s = card.snapshot ?? {}
  const ref = card.ref ?? {}
  const kind = str(ref.routine) ? 'routine' : 'job'
  const name = str(s.name) ?? 'Schedule'
  const { postedAt, now, live } = opts
  const paused = live ? live.paused : s.paused === true
  const every = str(s.every) ?? str(s.cron) ?? ''
  const nextAt = live ? live.nextRun : ms(s.nextRun)
  const snapshotLast = ms(s.lastRun)
  const last = live !== undefined && live !== null ? live.last : snapshotLast !== null ? { running: s.lastStatus === 'running', at: snapshotLast } : null
  const failed = !!live?.failed && !last?.running
  const runsAs = kind === 'routine' ? null : opts.viewerIsPerson ? 'you' : (opts.personName ?? 'you')
  const op = str(s.op)
  const trailing = postedWords(postedAt, now)

  const line = (text: string, done = false): ScheduleFold => ({ text, trailing, opens: false, done })
  let fold: ScheduleFold | null = null
  if (s.folded === 'changed') fold = line(`${name} · changed below`)
  else if (s.folded === 'removed') fold = line(`${name} · removed below`)
  else if (op === 'removed' || live === null) fold = line(`${name} · removed${runsAs === 'you' ? ' by you' : ''}`, true)
  else if (daysFrom(postedAt, now) < 0 && !opts.opened) {
    const next = paused ? 'paused' : nextAt !== null ? `next ${daysFrom(nextAt, now) === 0 ? clock(nextAt) : whenWords(nextAt, now)}` : null
    fold = { text: [name, quietEvery(every), next].filter(Boolean).join(' · '), trailing, opens: true, done: false }
  }

  return {
    kind,
    name,
    op: paused ? null : `${OPS[op ?? ''] ?? 'Added'} · ${trailing}`,
    paused,
    every,
    next: paused ? 'Paused' : nextAt !== null ? whenWords(nextAt, now) : '—',
    last: {
      text: !last ? 'Not run yet' : last.running ? `Running since ${clock(last.at)}` : whenWords(last.at, now),
      running: !!last?.running,
      failed,
      error: failed ? (str(live?.error) ?? null) : null,
    },
    runsAs,
    task: shortPaths(str(s.task) ?? ''),
    fold,
  }
}

/** A Kith job's live state from `kithInfo` schedules; null when kith answered without it (removed). */
export function liveKithJob(schedules: readonly KithSchedule[], id: string): ScheduleLive | null {
  const job = schedules.find((s) => s.id === id)
  if (!job) return null
  const running = job.lastStatus === 'running'
  return { paused: job.paused, nextRun: job.nextRun, last: job.lastRun !== null ? { running, at: job.lastRun } : null }
}

interface RoutineState { id: string; enabled: boolean; nextRunAt?: number | null; lastError?: string | null }
interface RoutineRunState { routineId: string; status: string; createdAt: number; finishedAt?: number | null }
const ACTIVE = ['pending', 'starting', 'running', 'recovering']

/** A routine's live state from the `routines` listing (runs newest first); null when it is gone. */
export function liveRoutine(listing: { routines: readonly RoutineState[]; runs: readonly RoutineRunState[] }, id: string): ScheduleLive | null {
  const routine = listing.routines.find((r) => r.id === id)
  if (!routine) return null
  const run = listing.runs.find((r) => r.routineId === id)
  const running = !!run && ACTIVE.includes(run.status)
  return {
    paused: !routine.enabled,
    nextRun: routine.nextRunAt ?? null,
    last: run ? { running, at: running ? run.createdAt : (run.finishedAt ?? run.createdAt) } : null,
    failed: !!run && ['failed', 'interrupted'].includes(run.status),
    error: routine.lastError ?? null,
  }
}
