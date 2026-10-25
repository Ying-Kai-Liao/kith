/**
 * What things cost at API prices (never a bill) and the monthly limits on agents and people: the host's usageReport,
 * agentUsage and agentPeople (serve/host/src/limits.rs, from kernel/bin/kith-cost), and the at-limit cards it adds.
 */
import type { Entry } from './models.ts'

export type Period = 'today' | 'week' | 'month'
/** What happens when an agent reaches its limit: a card (ask), a line (tell), or its runs wait (hold). */
export type AtLimit = 'ask' | 'tell' | 'hold'

export interface UsageRow {
  agent: string
  cost: number
  runs: number
  perRun: number
  limit: number | null
  at: AtLimit
  /** This calendar month's spend, when it has a limit (what the bar measures). */
  month: number | null
}

export interface UsageReport {
  period: Period
  /** "Today", "Last 7 days", "October". */
  label: string
  total: number
  agents: number
  own: number
  rows: UsageRow[]
}

export interface Forecast {
  perDay: number
  since: string
  text: string
  /** The day each preset amount is reached at this pace (null: not this month). */
  reach: Record<string, string | null>
}

export interface CostlyRun {
  session: string
  cost: number
  started: string
  via: string
  turn: string
}

export interface AgentUsage {
  agent: string
  period: Period
  label: string
  cost: number
  runs: number
  perRun: number
  /** This calendar month's spend and its name. */
  month: number
  monthLabel: string
  limit: number | null
  at: AtLimit
  days: { day: string; cost: number }[]
  models: { model: string; effort: string | null; cost: number; runs: number }[]
  costliest: CostlyRun[]
  forecast: Forecast | null
}

export interface AgentPeople {
  agent: string
  month: string
  /** What each person's runs cost this month; `owner` is the owner's own. */
  people: Record<string, number>
  /** Each person's budget on the agent, dollars a month; null for none. */
  budgets: Record<string, number | null>
}

const num = (v: unknown, or = 0) => (typeof v === 'number' && Number.isFinite(v) ? v : or)
const numOrNull = (v: unknown) => (typeof v === 'number' && Number.isFinite(v) ? v : null)
const str = (v: unknown, or = '') => (typeof v === 'string' ? v : or)
const obj = (v: unknown): Record<string, unknown> => (v && typeof v === 'object' && !Array.isArray(v) ? (v as Record<string, unknown>) : {})
const arr = (v: unknown): unknown[] => (Array.isArray(v) ? v : [])
const atOf = (v: unknown): AtLimit => (v === 'tell' || v === 'hold' ? v : 'ask')
const periodOf = (v: unknown): Period => (v === 'today' || v === 'week' ? v : 'month')

export function parseUsageReport(raw: unknown): UsageReport {
  const d = obj(raw)
  return {
    period: periodOf(d.period),
    label: str(d.label),
    total: num(d.total),
    agents: num(d.agents),
    own: num(d.own),
    rows: arr(d.rows).map((r) => {
      const o = obj(r)
      return { agent: str(o.agent), cost: num(o.cost), runs: num(o.runs), perRun: num(o.perRun), limit: numOrNull(o.limit), at: atOf(o.at), month: numOrNull(o.month) }
    }).filter((r) => r.agent),
  }
}

export function parseAgentUsage(raw: unknown): AgentUsage {
  const d = obj(raw)
  const f = obj(d.forecast)
  return {
    agent: str(d.agent),
    period: periodOf(d.period),
    label: str(d.label),
    cost: num(d.cost),
    runs: num(d.runs),
    perRun: num(d.perRun),
    month: num(d.month),
    monthLabel: str(d.monthLabel),
    limit: numOrNull(d.limit),
    at: atOf(d.at),
    days: arr(d.days).map((x) => ({ day: str(obj(x).day), cost: num(obj(x).cost) })),
    models: arr(d.models).map((x) => {
      const o = obj(x)
      return { model: str(o.model), effort: typeof o.effort === 'string' ? o.effort : null, cost: num(o.cost), runs: num(o.runs) }
    }),
    costliest: arr(d.costliest).map((x) => {
      const o = obj(x)
      return { session: str(o.session), cost: num(o.cost), started: str(o.started), via: str(o.via), turn: str(o.turn) }
    }),
    forecast: d.forecast && typeof f.text === 'string'
      ? { perDay: num(f.perDay), since: str(f.since), text: str(f.text), reach: Object.fromEntries(Object.entries(obj(f.reach)).map(([k, v]) => [k, typeof v === 'string' ? v : null])) }
      : null,
  }
}

export function parseAgentPeople(raw: unknown): AgentPeople {
  const d = obj(raw)
  return {
    agent: str(d.agent),
    month: str(d.month),
    people: Object.fromEntries(Object.entries(obj(d.people)).map(([k, v]) => [k, num(v)])),
    budgets: Object.fromEntries(Object.entries(obj(d.budgets)).map(([k, v]) => [k, numOrNull(v)])),
  }
}

/** "$6,667" from $10 up, "$2.92" below, or with `cents` always ("$289.01"). */
export function money(x: number, cents = false): string {
  if (!cents && Math.abs(x) >= 10) return `$${Math.round(x).toLocaleString('en-US')}`
  return `$${x.toLocaleString('en-US', { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`
}

/** "$289 of $300" (a person's own spend keeps its cents: "$20.10 of $20"). */
export const ofLimit = (spent: number, limit: number, cents = false) => `${money(spent, cents)} of ${money(limit)}`

/** How full a limit bar is, 0…1. */
export const fullness = (spent: number, limit: number | null) => (limit && limit > 0 ? Math.min(1, Math.max(0, spent / limit)) : 0)

/** "99 runs · $2.92 a run". */
export const runsLine = (runs: number, perRun: number) => `${runs} ${runs === 1 ? 'run' : 'runs'} · ${money(perRun)} a run`

/** The switch's labels: Today, Week and this month's name. */
export function periodChoices(now = new Date()): { id: Period; label: string }[] {
  return [
    { id: 'today', label: 'Today' },
    { id: 'week', label: 'Week' },
    { id: 'month', label: now.toLocaleString('en-US', { month: 'long' }) },
  ]
}

/** The rows the Usage page shows before "N more": the costliest first, as the report sorts them. */
export const SHOWN_AGENTS = 6

export const AT_LIMIT: { id: AtLimit; title: string; detail: string }[] = [
  { id: 'ask', title: 'Ask me', detail: 'A card: allow this run, raise the limit, or stop' },
  { id: 'tell', title: 'Tell me and keep going', detail: 'A line on Board; nothing waits' },
  { id: 'hold', title: 'Hold its runs', detail: 'They wait until next month or until you raise it' },
]

export const atLimitTitle = (at: AtLimit) => AT_LIMIT.find((a) => a.id === at)!.title.replace(' and keep going', '')

export const LIMIT_PRESETS = [100, 200, 300, 500] as const
export const BUDGET_PRESETS = [10, 20, 50] as const

const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec']

/** "Oct 24" from "2026-10-24" or "2026-10-24 00:04:08". */
export function shortDay(day: string): string {
  const m = /^(\d{4})-(\d{2})-(\d{2})/.exec(day)
  return m ? `${MONTHS[Number(m[2]) - 1]} ${Number(m[3])}` : day
}

/** The first of next month, for "starts again Nov 1". */
export function nextMonthStart(now = new Date()): string {
  return `${MONTHS[(now.getMonth() + 1) % 12]} 1`
}

const isoDay = (d: Date) => `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`

/** "today", "tomorrow" or "Oct 29". */
function dayWords(day: string, now: Date): string {
  const today = isoDay(now)
  const tomorrow = isoDay(new Date(now.getFullYear(), now.getMonth(), now.getDate() + 1))
  return day <= today ? 'today' : day === tomorrow ? 'tomorrow' : `about ${shortDay(day)}`
}

/**
 * The limit sheet's forecast in words: "At about $72 a day, $300 runs out today and $500 lasts to about Oct 29."
 * `chosen` null (no limit) says the pace alone. Days come from the ledger's own forecast.
 */
export function limitForecast(f: Forecast | null, spent: number, chosen: number | null, now = new Date()): string {
  if (!f || f.perDay <= 0) return chosen === null ? 'No limit: runs never wait for one.' : `${money(chosen)} a month; ${money(spent)} used so far.`
  const pace = `At about ${money(f.perDay)} a day`
  if (chosen === null) return `${pace}, no limit: runs never wait for one.`
  const say = (amount: number) => {
    if (spent >= amount) return `${money(amount)} is already used`
    const day = f.reach[String(amount)]
    return day ? `${money(amount)} runs out ${dayWords(day, now)}` : `${money(amount)} lasts the month`
  }
  // A limit that lasts the month is the whole answer; the next preset only matters when this one runs out.
  const lasts = spent < chosen && !f.reach[String(chosen)]
  const next = LIMIT_PRESETS.find((p) => p > chosen)
  if (lasts || next === undefined) return `${pace}, ${say(chosen)}.`
  const after = say(next).replace(' runs out ', ' lasts to ').replace('lasts to today', 'runs out today').replace('lasts to tomorrow', 'runs out tomorrow')
  return `${pace}, ${say(chosen)} and ${after}.`
}

/** "Opus 5.5 · high" from the model most of the cost ran on. */
export function modelLine(models: AgentUsage['models']): string {
  const top = [...models].sort((a, b) => b.cost - a.cost)[0]
  if (!top) return '—'
  const m = /claude-([a-z]+)-(\d+)-(\d+)/.exec(top.model)
  const name = m ? `${m[1]![0]!.toUpperCase()}${m[1]!.slice(1)} ${m[2]}.${m[3]}` : top.model
  return top.effort ? `${name} · ${top.effort}` : name
}

/** A costly run's first line, short. */
export function runTitle(turn: string): string {
  const line = turn.split('\n').find((l) => l.trim())?.trim() ?? 'A run'
  return line.length > 60 ? `${line.slice(0, 59)}…` : line
}

/** The limit a card carries (serve/host/src/limits.rs agent_card / guest_card). */
export interface LimitCardData {
  kind: 'agent' | 'person'
  agent: string
  person?: string
  spent: number
  limit: number
  month: string
  perRun?: number
  raiseTo?: number
  addTo?: number
  at?: AtLimit
}

export function limitOf(entry: Entry): LimitCardData | null {
  const l = obj((entry.data as Record<string, unknown>).limit)
  if (l.kind !== 'agent' && l.kind !== 'person') return null
  return {
    kind: l.kind,
    agent: str(l.agent),
    person: typeof l.person === 'string' ? l.person : undefined,
    spent: num(l.spent),
    limit: num(l.limit),
    month: str(l.month),
    perRun: numOrNull(l.perRun) ?? undefined,
    raiseTo: numOrNull(l.raiseTo) ?? undefined,
    addTo: numOrNull(l.addTo) ?? undefined,
    at: atOf(l.at),
  }
}

/** What a limit card shows, all words decided here. */
export interface LimitCardModel {
  /** "architect · Monthly limit" or "Mia · limit on video". */
  heading: string
  title: string
  /** "About $3 a run" or "6 runs this month". */
  left: string
  /** "$300.40 of $300". */
  right: string
  fullness: number
  /** The first is the dark one; a last `plain` one sits apart (Stop, Leave it). */
  options: { id: string; label: string; tone: 'ink' | 'pair' | 'plain' }[]
  footer: string | null
}

export function limitCardModel(entry: Entry, personName?: string, board = false): LimitCardModel | null {
  const l = limitOf(entry)
  if (!l) return null
  const d = entry.data
  const tone = (id: string, i: number): 'ink' | 'pair' | 'plain' => (id === 'stop' || id === 'leave' ? 'plain' : i === 0 ? 'ink' : 'pair')
  const options = (d.options ?? []).map((o, i) => ({ id: o.optionId, label: o.name, tone: tone(o.optionId, i) }))
  if (l.kind === 'person') {
    const name = personName || d.personName || l.person || 'Someone'
    return {
      heading: `${name} · limit on ${l.agent}`,
      title: d.title ?? `${name} reached their limit.`,
      left: `${l.month} at API prices`,
      right: ofLimit(l.spent, l.limit, true),
      fullness: fullness(l.spent, l.limit),
      options,
      footer: null,
    }
  }
  return {
    heading: `${l.agent} · Monthly limit`,
    // On Board the card stands away from its chat, so it says which run: the one waiting.
    title: board ? limitQuestion(entry)! : (d.title ?? `${l.agent} has used its ${money(l.limit)} for ${l.month}.`),
    left: l.perRun ? `About $${Math.max(1, Math.round(l.perRun))} a run` : `${l.month} at API prices`,
    right: ofLimit(l.spent, l.limit, true),
    fullness: fullness(l.spent, l.limit),
    options,
    footer: d.status === 'pending' && !board ? 'Your message is kept. Nothing runs until you answer.' : null,
  }
}

/** The answered card's one line. */
export function limitOutcome(entry: Entry): string {
  const d = entry.data
  if (d.status === 'cancelled') return 'Closed'
  const chosen = d.options?.find((o) => o.optionId === d.selected)
  switch (d.selected) {
    case 'allow':
      return 'Allowed this run'
    case 'stop':
      return 'Stopped · your message is kept'
    case 'leave':
      return 'Left as it is'
    case 'raise':
    case 'add':
      return chosen ? chosen.name.replace(/^Raise/, 'Raised').replace(/^Add/, 'Added') : 'Raised'
    default:
      return chosen?.name ?? 'Answered'
  }
}

/** Board's line for a waiting limit card. */
export function limitQuestion(entry: Entry): string | null {
  const l = limitOf(entry)
  if (!l) return null
  return l.kind === 'agent' ? `${l.agent} used its ${money(l.limit)} for ${l.month}. One run is waiting.` : `${entry.data.personName || l.person} reached their ${money(l.limit)} for ${l.month}.`
}

/** The person rows of a shared agent: what they spent this month against their budget. */
export function personSpend(people: AgentPeople | null, personId: string): { spent: number; budget: number | null } {
  return { spent: people?.people[personId] ?? 0, budget: people?.budgets[personId] ?? null }
}
