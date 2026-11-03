// The managed account set (`accounts` and its writes on the host): the kernel owns the set, its order, the policy, the
// pins and which accounts are out; the app shows that and sends whole lists back. Every write answers with the same read.
// Times are epoch milliseconds. The app never handles a token: logins happen in a terminal the host opens.

export type AccountStatus = 'ready' | 'out' | 'signed_out' | 'unsupported' | 'invalid'
export type Policy = 'fill' | 'spread'

export interface UsageWindow {
  name: string
  percent: number
  resetsAt: number | null
}

export interface AccountPin {
  agent: string
  only: boolean
}

export interface Account {
  /** `claude`, `claude:work`, `codex`, `codex:deep`. */
  id: string
  harness: string
  account: string | null
  kind: string
  inSet: boolean
  /** Position in the global set; null when not in it. */
  order: number | null
  status: AccountStatus
  ready: boolean
  why: string | null
  /** The one command that makes a signed-out account ready. */
  fix: string | null
  outUntil: number | null
  reason: string | null
  usage: { readAt: number | null; windows: UsageWindow[] } | null
  lastStarted: number | null
  pins: AccountPin[]
  loggedIn: boolean
}

export interface Skipped {
  id: string
  status: string
  why: string | null
  outUntil: number | null
  reason: string | null
}

export interface AgentAccounts {
  harness: string | null
  pin: { id: string; only: boolean } | null
  /** The agent's own list; null: it follows the global set. */
  own: string[] | null
  /** Its static order, pin first. */
  order: string[]
  main: string | null
  /** What a run would try now. */
  candidates: string[]
  allOut: boolean
  crossHarness: boolean
  skipped: Skipped[]
}

export interface Accounts {
  policy: Policy
  crossHarness: boolean
  accounts: Account[]
  /** Only the agents whose view could be read. */
  agents: Record<string, AgentAccounts>
}

export const emptyAccounts: Accounts = { policy: 'fill', crossHarness: false, accounts: [], agents: {} }

/** What `accountAdd` answers: the warning to confirm first, or the login's terminal. */
export type AccountAdd = { kind: 'warning'; warning: string } | { kind: 'term'; term: string; id: string }

type Raw = Record<string, unknown>
const isObject = (v: unknown): v is Raw => !!v && typeof v === 'object' && !Array.isArray(v)
const str = (v: unknown): string | null => (typeof v === 'string' ? v : null)
const num = (v: unknown): number | null => (typeof v === 'number' && Number.isFinite(v) ? v : null)
const strings = (v: unknown): string[] => (Array.isArray(v) ? v.filter((i): i is string => typeof i === 'string') : [])
const STATUSES: readonly string[] = ['ready', 'out', 'signed_out', 'unsupported', 'invalid']

function parseAccount(e: Raw): Account | null {
  const id = str(e.id)
  if (!id) return null
  const usage = isObject(e.usage) && Array.isArray(e.usage.windows)
    ? {
        readAt: num(e.usage.readAt),
        windows: e.usage.windows.filter(isObject).flatMap((w): UsageWindow[] => {
          const name = str(w.name)
          const percent = num(w.percent)
          return name && percent !== null ? [{ name, percent, resetsAt: num(w.resetsAt) }] : []
        }),
      }
    : null
  const status = STATUSES.includes(e.status as string) ? (e.status as AccountStatus) : 'invalid'
  return {
    id,
    harness: str(e.harness) ?? id.split(':')[0] ?? '',
    account: str(e.account),
    kind: str(e.kind) ?? 'subscription',
    inSet: e.inSet === true,
    order: num(e.order),
    status,
    ready: e.ready === true,
    why: str(e.why),
    fix: str(e.fix),
    outUntil: num(e.outUntil),
    reason: str(e.reason),
    usage,
    lastStarted: num(e.lastStarted),
    pins: (Array.isArray(e.pins) ? e.pins : []).filter(isObject).flatMap((p): AccountPin[] => {
      const agent = str(p.agent)
      return agent ? [{ agent, only: p.only === true }] : []
    }),
    loggedIn: e.loggedIn !== false,
  }
}

function parseAgent(e: Raw): AgentAccounts {
  const pin = isObject(e.pin) && str(e.pin.id) ? { id: str(e.pin.id)!, only: e.pin.only === true } : null
  return {
    harness: str(e.harness),
    pin,
    own: Array.isArray(e.own) ? strings(e.own) : null,
    order: strings(e.order),
    main: str(e.main),
    candidates: strings(e.candidates),
    allOut: e.allOut === true,
    crossHarness: e.crossHarness === true,
    skipped: (Array.isArray(e.skipped) ? e.skipped : []).filter(isObject).flatMap((s): Skipped[] => {
      const id = str(s.id)
      return id ? [{ id, status: str(s.status) ?? 'invalid', why: str(s.why), outUntil: num(s.outUntil), reason: str(s.reason) }] : []
    }),
  }
}

export function parseAccounts(raw: unknown): Accounts {
  const r = isObject(raw) ? raw : {}
  const agents: Record<string, AgentAccounts> = {}
  if (isObject(r.agents)) for (const [name, v] of Object.entries(r.agents)) if (isObject(v)) agents[name] = parseAgent(v)
  return {
    policy: r.policy === 'spread' ? 'spread' : 'fill',
    crossHarness: r.crossHarness === true,
    accounts: (Array.isArray(r.accounts) ? r.accounts : []).filter(isObject).flatMap((e) => parseAccount(e) ?? []),
    agents,
  }
}

/** `accountAdd`'s answer: `{ok: false, warning}` first for a new Claude account, else the login's terminal. */
export function parseAccountAdd(raw: unknown): AccountAdd {
  const r = isObject(raw) ? raw : {}
  if (r.ok === false) {
    const warning = str(r.warning)
    if (warning) return { kind: 'warning', warning }
    throw new Error(str(r.error) ?? 'The computer would not add the account.')
  }
  const term = str(r.term)
  const id = isObject(r.account) ? str(r.account.id) : null
  if (!term || !id) throw new Error('The computer answered without a login terminal.')
  return { kind: 'term', term, id }
}

/** The global set's ids, in its order. */
export const setIds = (a: Accounts): string[] =>
  a.accounts.filter((x) => x.inSet).sort((x, y) => (x.order ?? 0) - (y.order ?? 0)).map((x) => x.id)

export const accountFor = (a: Accounts, id: string): Account | undefined => a.accounts.find((x) => x.id === id)

/** Accounts not in `ids`, for adding: ready ones first. */
export function addableTo(a: Accounts, ids: string[]): Account[] {
  return a.accounts.filter((x) => !ids.includes(x.id)).sort((x, y) => Number(y.ready) - Number(x.ready))
}

/** `ids` with the entry at `from` moved to `to`. */
export function move(ids: string[], from: number, to: number): string[] {
  if (from === to || from < 0 || to < 0 || from >= ids.length || to >= ids.length) return ids
  const next = [...ids]
  next.splice(to, 0, next.splice(from, 1)[0]!)
  return next
}

export const withId = (ids: string[], id: string): string[] => (ids.includes(id) ? ids : [...ids, id])
export const withoutId = (ids: string[], id: string): string[] => ids.filter((i) => i !== id)

/** Every agent the page should list: those it could read, then those only a pin names (their view could not be read). */
export function agentNames(a: Accounts): { name: string; view: AgentAccounts | null }[] {
  const names = new Set(Object.keys(a.agents))
  for (const x of a.accounts) for (const p of x.pins) names.add(p.agent)
  return [...names].sort().map((name) => ({ name, view: a.agents[name] ?? null }))
}

const clock = new Intl.DateTimeFormat(undefined, { hour: 'numeric', minute: '2-digit' })
const dayClock = new Intl.DateTimeFormat(undefined, { month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit' })

/** "3:40 PM", or with the day when it isn't today. */
export function timeText(ms: number, now = Date.now()): string {
  const day = (t: number) => new Date(t).toDateString()
  return (day(ms) === day(now) ? clock : dayClock).format(ms)
}

export function reasonText(reason: string | null): string | null {
  if (reason === 'rate_limit') return 'The provider refused a turn.'
  if (reason === 'usage_95') return 'Switched early, at 95% of its usage.'
  return null
}

export type Tone = 'ok' | 'out' | 'blocked'

/** One short line for an account's (or a skipped entry's) state. */
export function statusText(
  e: { status: string; why: string | null; outUntil: number | null; reason: string | null },
  now = Date.now(),
): { text: string; tone: Tone } {
  switch (e.status) {
    case 'ready':
      return { text: 'ready', tone: 'ok' }
    case 'out': {
      const until = e.outUntil !== null ? `out until ${timeText(e.outUntil, now)}` : 'out of usage'
      const why = reasonText(e.reason)
      return { text: why ? `${until}. ${why}` : until, tone: 'out' }
    }
    case 'signed_out':
      return { text: e.why ?? 'signed out', tone: 'blocked' }
    case 'unsupported':
      return { text: e.why ?? 'not supported on this computer', tone: 'blocked' }
    default:
      return { text: e.why ?? 'not valid', tone: 'blocked' }
  }
}

/** "5-hour: 42%, resets 3:40 PM". */
export function windowText(w: UsageWindow, now = Date.now()): string {
  const used = `${w.name}: ${Math.round(w.percent)}%`
  return w.resetsAt !== null ? `${used}, resets ${timeText(w.resetsAt, now)}` : used
}

/** The pin of `agent` on an account, for the account's row. */
export const pinText = (p: AccountPin) => (p.only ? `${p.agent} (only)` : p.agent)

/** The id an added login gets. */
export const loginId = (harness: string, name: string) => `${harness}:${name}`

/** Whether `id` is an extra login (has a name) rather than a harness's own login, which cannot be removed. */
export const isExtra = (a: Account) => a.account !== null && a.account !== ''
