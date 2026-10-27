// A Kith bot's detail page: what `kith <agent> --info-json` prints, as the host's `kithInfo`,
// and `kithMemorySearch` pass it on (serve/docs/bot-info.md). Lenient on purpose:
// a kith a little newer or older than the app still draws what it can.

export interface KithRoleSection { title: string; text: string }
export interface KithSkill { name: string; description: string; source: string }
export interface KithMcp { name: string; kind: string; command: string | null; url: string | null }
export interface KithProject { id: string; files: number; modified: number | null }
export interface KithSchedule {
  id: string
  cron: string
  task: string
  paused: boolean
  lastRun: number | null
  /** `running`, `done` or null (never ran). */
  lastStatus: string | null
  nextRun: number | null
}
export interface KithCost {
  today: number
  week: number
  month: number
  all: number
  requestsMonth: number
  byModelMonth: { model: string; usd: number }[]
}
export interface KithMemoryEntry { path: string; bytes: number; modified: number | null }
export interface KithPackage {
  version: string | null
  remote: string | null
  dirty: boolean
  unpushed: boolean
  pinned: string | null
}

export interface KithInfo {
  agent: string
  description: string
  harness: string | null
  model: string | null
  effort: string | null
  permissionMode: string | null
  /** The model a voice call runs on, as this Mac chose it: a model name, `own`, or `default` (kith's default, sonnet). */
  callModel: string | null
  /** What `default` means on this Mac (the global call model; `own` is the agent's own). */
  callModelDefault: string | null
  readOnly: boolean
  role: { intro: string; sections: KithRoleSection[] } | null
  package: KithPackage | null
  skills: KithSkill[]
  tools: string[]
  mcpServers: KithMcp[]
  plugins: string[]
  projects: KithProject[]
  schedules: KithSchedule[]
  cost: KithCost | null
  memory: { files: KithMemoryEntry[]; totalBytes: number } | null
  /** Parts kith could not read: key → reason. */
  errors: Record<string, string>
}

/** The host's `kithInfo` answer. */
export interface KithInfoResult { info: KithInfo | null; error: string | null; fetchedAt: number | null }

export interface KithMemoryHit { path: string; line: number | null; snippet: string; openable: boolean; agent: string | null }
export interface KithSearchResult { hits: KithMemoryHit[]; error: string | null }

type Raw = Record<string, unknown>
const isObject = (v: unknown): v is Raw => !!v && typeof v === 'object' && !Array.isArray(v)
const str = (v: unknown): string | null => (typeof v === 'string' ? v : null)
const num = (v: unknown): number | null => (typeof v === 'number' && Number.isFinite(v) ? v : null)
const list = (v: unknown): unknown[] => (Array.isArray(v) ? v : [])
const objects = (v: unknown): Raw[] => list(v).filter(isObject)

/** Unix ms from a number (seconds or milliseconds) or an ISO date string; null when it is neither. */
export function toMillis(v: unknown): number | null {
  if (typeof v === 'number' && Number.isFinite(v)) return v < 1e11 ? v * 1000 : v
  if (typeof v === 'string' && v) {
    const ms = Date.parse(v)
    return Number.isNaN(ms) ? null : ms
  }
  return null
}

export function parseKithInfo(raw: unknown): KithInfo | null {
  if (!isObject(raw)) return null
  const role = isObject(raw.role) ? raw.role : null
  const pkg = isObject(raw.package) ? raw.package : null
  const cost = isObject(raw.cost) ? raw.cost : null
  const memory = isObject(raw.memory) ? raw.memory : null
  const errors: Record<string, string> = {}
  if (isObject(raw.errors)) for (const [k, v] of Object.entries(raw.errors)) errors[k] = typeof v === 'string' ? v : String(v)
  const files = memory ? objects(memory.files).flatMap((f) => {
    const path = str(f.path)
    return path ? [{ path, bytes: num(f.bytes) ?? 0, modified: toMillis(f.modified) }] : []
  }) : []
  return {
    agent: str(raw.agent) ?? '',
    description: str(raw.description) ?? '',
    harness: str(raw.harness),
    model: str(raw.model),
    effort: str(raw.effort),
    permissionMode: str(raw.permission_mode),
    callModel: str(raw.call_model),
    callModelDefault: str(raw.call_model_default),
    readOnly: raw.read_only === true || raw.readonly === true,
    role: role ? {
      intro: str(role.intro) ?? '',
      sections: objects(role.sections).flatMap((s) => (str(s.title) !== null ? [{ title: str(s.title)!, text: str(s.text) ?? '' }] : [])),
    } : null,
    package: pkg ? {
      version: str(pkg.version),
      remote: str(pkg.remote),
      dirty: pkg.dirty === true,
      unpushed: pkg.unpushed === true,
      pinned: str(pkg.pinned),
    } : null,
    skills: objects(raw.skills).flatMap((s) => (str(s.name) ? [{ name: str(s.name)!, description: str(s.description) ?? '', source: str(s.source) ?? 'package' }] : [])),
    tools: list(raw.tools).filter((t): t is string => typeof t === 'string'),
    mcpServers: objects(raw.mcp_servers).flatMap((m) => (str(m.name) ? [{ name: str(m.name)!, kind: str(m.kind) ?? '', command: str(m.command), url: str(m.url) }] : [])),
    plugins: list(raw.plugins).filter((t): t is string => typeof t === 'string'),
    projects: objects(raw.projects).flatMap((p) => (str(p.id) ? [{ id: str(p.id)!, files: num(p.files) ?? 0, modified: toMillis(p.modified) }] : [])),
    schedules: objects(raw.schedules).flatMap((s) => (str(s.id) !== null ? [{
      id: str(s.id)!,
      cron: str(s.cron) ?? '',
      task: str(s.task) ?? '',
      paused: s.paused === true,
      lastRun: toMillis(s.last_run),
      lastStatus: str(s.last_status),
      nextRun: toMillis(s.next_run),
    }] : [])),
    cost: cost ? {
      today: num(cost.today) ?? 0,
      week: num(cost.week) ?? 0,
      month: num(cost.month) ?? 0,
      all: num(cost.all) ?? 0,
      requestsMonth: num(cost.requests_month) ?? 0,
      byModelMonth: objects(cost.by_model_month).flatMap((m) => (str(m.model) ? [{ model: str(m.model)!, usd: num(m.usd) ?? 0 }] : [])),
    } : null,
    memory: memory ? { files, totalBytes: num(memory.total_bytes) ?? files.reduce((n, f) => n + f.bytes, 0) } : null,
    errors,
  }
}

export function parseKithInfoResult(raw: unknown): KithInfoResult {
  const r = isObject(raw) ? raw : {}
  return { info: parseKithInfo(r.info), error: str(r.error), fetchedAt: num(r.fetchedAt) }
}

/** One "always" answer to an escape card that kith-auth keeps (`kithAccessGrants`): `person` may `kind` `target` beyond
 *  the box in every run, until it is removed. `since` is null for one kith-auth kept no time for. */
export interface AccessGrant { person: string; name: string; kind: 'read' | 'write' | 'domain'; target: string; since: number | null }

export function parseAccessGrants(raw: unknown): AccessGrant[] {
  const r = isObject(raw) ? raw : {}
  return objects(r.grants).flatMap((g) => {
    const kind = str(g.kind)
    const person = str(g.person)
    const target = str(g.target)
    if ((kind !== 'read' && kind !== 'write' && kind !== 'domain') || !person || !target) return []
    return [{ person, name: str(g.name) ?? '', kind, target, since: num(g.since) }]
  })
}

/** What a grant lets them do, in words: "Read ~/docs", "Read and write ~/notes", "Connect to pypi.org". */
export function accessGrantText(g: Pick<AccessGrant, 'kind' | 'target'>): string {
  return g.kind === 'domain' ? `Connect to ${g.target}` : `${g.kind === 'write' ? 'Read and write' : 'Read'} ${g.target}`
}

/** Who holds it and since when: "Sam · since 3 Oct". The everyone row (`*`) is "Everyone". */
export function accessGrantHolder(g: Pick<AccessGrant, 'person' | 'name' | 'since'>): string {
  const who = g.person === '*' ? 'Everyone' : g.name || g.person
  return g.since === null ? who : `${who} · since ${new Date(g.since).toLocaleDateString([], { day: 'numeric', month: 'short', year: 'numeric' })}`
}

export function parseSearchResult(raw: unknown): KithSearchResult {
  const r = isObject(raw) ? raw : {}
  const hits = objects(r.hits).flatMap((h) => (str(h.path) ? [{
    path: str(h.path)!,
    line: num(h.line),
    snippet: str(h.snippet) ?? '',
    openable: h.openable === true,
    agent: str(h.agent),
  }] : []))
  return { hits, error: str(r.error) }
}

/** The package fingerprint is a hash: the first 7 characters name it. */
export const shortVersion = (version: string | null | undefined) => (version ? version.slice(0, 7) : null)

/** What the package chip's tooltip says about where the package stands. */
export function packageNotes(pkg: KithPackage): string[] {
  return [
    ...(pkg.remote ? [pkg.remote] : []),
    ...(pkg.dirty ? ['uncommitted changes'] : []),
    ...(pkg.unpushed ? ['not pushed'] : []),
    ...(pkg.pinned ? [`pinned to ${pkg.pinned}`] : []),
  ]
}

/** USD with two decimals; anything above zero but under a cent reads "<$0.01". */
export function formatUsd(usd: number): string {
  if (usd > 0 && usd < 0.005) return '<$0.01'
  return `$${usd.toFixed(2)}`
}

/** The month's top models with their share of the month's spend. */
export function topModels(cost: KithCost, n = 3): { model: string; usd: number; share: number }[] {
  const total = cost.byModelMonth.reduce((sum, m) => sum + m.usd, 0) || cost.month
  return [...cost.byModelMonth]
    .sort((a, b) => b.usd - a.usd)
    .slice(0, n)
    .map((m) => ({ ...m, share: total > 0 ? m.usd / total : 0 }))
}

const clock = (h: number, m: number) => `${String(h).padStart(2, '0')}:${String(m).padStart(2, '0')}`

/** A simple cron in plain words; anything else comes back as the cron itself. */
export function cronWords(cron: string): string {
  const f = cron.trim().split(/\s+/)
  if (f.length !== 5) return cron
  const [min, hour, dom, mon, dow] = f
  const int = (s: string, max: number) => (/^\d+$/.test(s) && Number(s) <= max ? Number(s) : null)
  const every = /^\*\/(\d+)$/.exec(min)
  if (every && hour === '*' && dom === '*' && mon === '*' && dow === '*' && Number(every[1]) > 0) {
    return Number(every[1]) === 1 ? 'every minute' : `every ${every[1]} minutes`
  }
  const m = int(min, 59)
  if (m === null || dom !== '*' || mon !== '*') return cron
  if (hour === '*' && dow === '*') return m === 0 ? 'hourly' : `hourly at :${String(m).padStart(2, '0')}`
  const h = int(hour, 23)
  if (h === null) return cron
  if (dow === '*') return `daily at ${clock(h, m)}`
  if (dow === '1-5') return `weekdays at ${clock(h, m)}`
  return cron
}

/** "2 h ago" style age; `now` is injectable for tests. */
export function ago(ms: number, now = Date.now()): string {
  const s = Math.max(0, (now - ms) / 1000)
  if (s < 60) return 'just now'
  if (s < 3600) return `${Math.floor(s / 60)} min ago`
  if (s < 86_400) return `${Math.floor(s / 3600)} h ago`
  if (s < 86_400 * 60) return `${Math.floor(s / 86_400)} d ago`
  return `${Math.floor(s / (86_400 * 30))} mo ago`
}

/** How a schedule's last run reads: "running", "2 h ago" or null (never ran). */
export function lastRunText(s: KithSchedule, now = Date.now()): string | null {
  if (s.lastStatus === 'running') return 'running'
  return s.lastRun !== null ? ago(s.lastRun, now) : null
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(bytes < 10 * 1024 ? 1 : 0)} KB`
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`
}

/** The "Not yours" section is the bot's boundary: a card of its own, apart from the other sections. */
export function splitRoleSections(sections: KithRoleSection[]): { notYours: KithRoleSection[]; rest: KithRoleSection[] } {
  const isBoundary = (s: KithRoleSection) => s.title.trim().toLowerCase() === 'not yours'
  return { notYours: sections.filter(isBoundary), rest: sections.filter((s) => !isBoundary(s)) }
}

/** The whole role prompt as one markdown text: the intro, then every section under its title. Null when there is none. */
export function rolePrompt(role: { intro: string; sections: KithRoleSection[] } | null): string | null {
  if (!role) return null
  const parts = [role.intro.trim(), ...role.sections.map((s) => `## ${s.title}\n\n${s.text.trim()}`)].filter(Boolean)
  return parts.length ? parts.join('\n\n') : null
}

/** An older host answers an unknown method with 404: the kith sections stay hidden then. */
export function isUnsupportedHost(error: unknown): boolean {
  const e = error as { status?: number; message?: string } | null
  return e?.status === 404 || /unknown method/i.test(e?.message ?? '')
}
