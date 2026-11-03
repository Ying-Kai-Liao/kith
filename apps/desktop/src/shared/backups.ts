// The backup chain: the accounts and providers Kith moves a bot's turns to when its own account runs out of usage.
// The host's `backups` answers with the order and each entry's state; `backupsSet` replaces a whole order. Above the
// chain is each agent's main account, the one its turns start on (`accountSet` chooses it). The app only shows state
// and the one command that fixes an entry: logging in is the user's step in the terminal.

export interface BackupEntry {
  /** `claude`, `claude:work`, `claude-api:main`, `codex`, `codex:deep`. */
  id: string
  harness: string | null
  account: string | null
  /** `subscription` or `api-key`. */
  kind: string
  ready: boolean
  /** Why it isn't ready. */
  why: string | null
  /** The one command that makes it ready. */
  fix: string | null
  /** Epoch ms the entry is back, while it is out of usage. */
  outUntil: number | null
  /** `rate_limit` (the provider refused a turn) or `usage_95` (switched early, before the limit). */
  reason: string | null
}

/** The account an agent's turns start on: chain entry 0. Its stored session belongs to it. */
export interface MainAccount {
  /** An entry id of the agent's harness: `claude`, `claude:work`, `claude-api:main`, `codex`, `codex:deep`. */
  id: string
  /** `agent` (its own choice), `global` (the computer's) or `default` (the harness's own login). */
  source: string
  /** The agent's harness, which decides the ids it can take. */
  harness: string | null
  ready: boolean
  why: string | null
  fix: string | null
}

export interface Backups {
  /** Entry ids, in order. */
  global: string[]
  /** Only the agents with an order of their own. */
  agents: Record<string, string[]>
  /** Every entry known: in an order, or an account that could be added. */
  entries: BackupEntry[]
  /** The computer's main account (null: each harness's own login) and every agent's, whatever it comes from. */
  main: { global: string | null; agents: Record<string, MainAccount> }
}

export const emptyBackups: Backups = { global: [], agents: {}, entries: [], main: { global: null, agents: {} } }

type Raw = Record<string, unknown>
const isObject = (v: unknown): v is Raw => !!v && typeof v === 'object' && !Array.isArray(v)
const str = (v: unknown): string | null => (typeof v === 'string' ? v : null)
const ids = (v: unknown): string[] => (Array.isArray(v) ? v.filter((i): i is string => typeof i === 'string') : [])

export function parseBackups(raw: unknown): Backups {
  const r = isObject(raw) ? raw : {}
  const agents: Record<string, string[]> = {}
  if (isObject(r.agents)) for (const [name, chain] of Object.entries(r.agents)) agents[name] = ids(chain)
  const entries = (Array.isArray(r.entries) ? r.entries : []).filter(isObject).flatMap((e): BackupEntry[] => {
    const id = str(e.id)
    if (!id) return []
    return [{
      id,
      harness: str(e.harness),
      account: str(e.account),
      kind: str(e.kind) ?? 'subscription',
      ready: e.ready === true,
      why: str(e.why),
      fix: str(e.fix),
      outUntil: typeof e.outUntil === 'number' && Number.isFinite(e.outUntil) ? e.outUntil : null,
      reason: str(e.reason),
    }]
  })
  const m = isObject(r.main) ? r.main : {}
  const mains: Record<string, MainAccount> = {}
  if (isObject(m.agents)) {
    for (const [name, e] of Object.entries(m.agents)) {
      const id = isObject(e) ? str(e.id) : null
      if (!isObject(e) || !id) continue
      mains[name] = { id, source: str(e.source) ?? 'default', harness: str(e.harness), ready: e.ready !== false, why: str(e.why), fix: str(e.fix) }
    }
  }
  return { global: ids(r.global), agents, entries, main: { global: str(m.global), agents: mains } }
}

/** Entries always offered for adding: a provider's own login, and the other kinds when the user has made one. */
export const QUICK_ADD = ['codex', 'claude'] as const

/** "via codex", "via claude:work": which entry of the chain answered. Nothing for the primary and for old messages. */
export function viaLabel(answeredBy: unknown): string | null {
  return typeof answeredBy === 'string' && answeredBy ? `via ${answeredBy}` : null
}

/** The entry for `id`; one the host hasn't described (just added) is a plain, unknown one. */
export function entryFor(backups: Backups, id: string): BackupEntry {
  return backups.entries.find((e) => e.id === id) ?? {
    id, harness: id.split(':')[0] ?? null, account: id.split(':')[1] ?? null, kind: 'subscription',
    ready: false, why: null, fix: null, outUntil: null, reason: null,
  }
}

/** Whether the entry is out of usage right now. */
export const isOut = (e: BackupEntry, now = Date.now()) => e.outUntil !== null && e.outUntil > now

const clock = new Intl.DateTimeFormat(undefined, { hour: 'numeric', minute: '2-digit' })
const dayClock = new Intl.DateTimeFormat(undefined, { month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit' })

/** "back at 3:40 PM", or with the day when it isn't today. */
export function backAt(ms: number, now = Date.now()): string {
  const day = (t: number) => new Date(t).toDateString()
  return `back at ${(day(ms) === day(now) ? clock : dayClock).format(ms)}`
}

/** The short state of one entry: what the user needs to know, not a status code. */
export function stateText(e: BackupEntry, now = Date.now()): { text: string; tone: 'ok' | 'out' | 'blocked' } {
  if (isOut(e, now)) return { text: `out of usage, ${backAt(e.outUntil!, now)}`, tone: 'out' }
  if (!e.ready) return { text: e.why ?? 'not ready', tone: 'blocked' }
  return { text: 'ready', tone: 'ok' }
}

/** Why an entry is out, in words. */
export function reasonText(reason: string | null): string | null {
  if (reason === 'rate_limit') return 'The provider refused a turn.'
  if (reason === 'usage_95') return 'Switched early, at 95% of its usage.'
  return null
}

/** Whether the order holds an extra Claude subscription account (`claude:<name>`), which the policy note is about. */
export const hasClaudeAccount = (chain: string[]) => chain.some((id) => id.startsWith('claude:'))

/** `chain` with the entry at `from` moved to `to`. */
export function move(chain: string[], from: number, to: number): string[] {
  if (from === to || from < 0 || to < 0 || from >= chain.length || to >= chain.length) return chain
  const next = [...chain]
  next.splice(to, 0, next.splice(from, 1)[0]!)
  return next
}

/** What can still be added: every known entry not in `chain`, ready first, plus the plain providers. */
export function addable(backups: Backups, chain: string[]): string[] {
  const known = backups.entries.map((e) => e.id)
  const all = [...new Set([...known, ...QUICK_ADD])].filter((id) => !chain.includes(id))
  const ready = (id: string) => (backups.entries.find((e) => e.id === id)?.ready ? 0 : 1)
  return all.sort((a, b) => ready(a) - ready(b))
}

/** The harness an entry runs on as a main account: `claude-api:main` is a Claude one. */
export const familyOf = (id: string) => (id.split(':')[0] ?? id).replace(/-api$/, '')

/**
 * What can be an agent's main account: the known entries of its harness (another harness stays a backup's job),
 * ready first. Without a harness (the computer's setting, for every agent) any entry; an agent of another harness
 * keeps its own login.
 */
export function mainChoices(backups: Backups, harness: string | null): string[] {
  const known = [...new Set([...backups.entries.map((e) => e.id), ...QUICK_ADD])]
  const fits = known.filter((id) => !harness || familyOf(id) === harness)
  const ready = (id: string) => (backups.entries.find((e) => e.id === id)?.ready ? 0 : 1)
  return fits.sort((a, b) => ready(a) - ready(b))
}

/** Where an agent's main account comes from, in words. */
export function mainSourceText(m: MainAccount): string {
  if (m.source === 'agent') return "this bot's own choice"
  if (m.source === 'global') return "the computer's main account"
  return 'the default login'
}

/** The main account as a chain entry: its state and fix come from the main row, which knows the agent. */
export const mainEntry = (backups: Backups, m: MainAccount): BackupEntry =>
  ({ ...entryFor(backups, m.id), ready: m.ready, why: m.why, fix: m.fix, harness: m.harness })

/** The command that makes an unknown plain provider ready, for an entry the host hasn't described yet. */
export const loginCommand = (e: BackupEntry) => e.fix ?? (e.id === 'codex' ? 'codex login' : e.id === 'claude' ? 'claude auth login' : null)

/** Policy text shown whenever a Claude subscription account is in an order. */
export const CLAUDE_ACCOUNTS_NOTE =
  "Anthropic's usage policy doesn't allow using several Claude accounts to get past usage limits; accounts used that way can be suspended. Whether to do this is your decision. A Claude API key or another provider such as Codex is an allowed way to get more room."
