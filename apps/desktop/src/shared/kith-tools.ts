// Connectors, skills and shared bots on a kith agent: what `kith <agent> connectors|skills --json`, `kith market --json`
// and `kith install --preview --json` print, as the host's kith* methods pass them on. Lenient on purpose, like
// kith-info.ts: a kith a little newer or older than the app still draws what it can.

import type { Bot } from './models.ts'

const str = (v: unknown): string => (typeof v === 'string' ? v : '')
const strs = (v: unknown): string[] => (Array.isArray(v) ? v.filter((x): x is string => typeof x === 'string') : [])
const rows = (v: unknown): Record<string, unknown>[] => (Array.isArray(v) ? v.filter((x) => x && typeof x === 'object') : [])

/** One connector as an agent has it (`kith <agent> connectors --json`). */
export interface KithConnector {
  name: string
  description: string
  profile: string | null
  /** ok, missing secrets, signed out, no such profile, unknown service, not granted. */
  status: string
  missing: string[]
  /** The agent's package asks for it (`needs:`), so taking it off here would not stop the agent wanting it. */
  declared: boolean
}

/** One skill of an agent (`kith <agent> skills --json`): own and learned ones are the agent's; only linked ones come off. */
export interface KithAgentSkill {
  name: string
  kind: string
  description: string
}

export interface KithTools {
  connectors: KithConnector[]
  skills: KithAgentSkill[]
  connectorsError: string | null
  skillsError: string | null
}

export function parseTools(raw: unknown): KithTools {
  const r = (raw && typeof raw === 'object' ? raw : {}) as Record<string, unknown>
  return {
    connectors: rows(r.connectors).map((c) => ({
      name: str(c.name),
      description: str(c.description),
      profile: str(c.profile) || null,
      status: str(c.status) || 'ok',
      missing: strs(c.missing),
      declared: c.declared === true,
    })).filter((c) => c.name),
    skills: rows(r.skills).map((s) => ({ name: str(s.name), kind: str(s.kind), description: str(s.description) })).filter((s) => s.name),
    connectorsError: str(r.connectorsError) || null,
    skillsError: str(r.skillsError) || null,
  }
}

/** A connector row of `kith connectors --json`: what this Mac can connect, and which agents have it. */
export interface AvailableConnector {
  name: string
  display: string | null
  auth: string | null
  connected: string[]
}

export function parseAvailable(raw: unknown): AvailableConnector[] {
  return rows(raw).map((c) => ({ name: str(c.name), display: str(c.display) || null, auth: str(c.auth) || null, connected: strs(c.connected) })).filter((c) => c.name)
}

/** The words for a connector's status on an agent's list. */
export function connectorStatusText(c: Pick<KithConnector, 'status' | 'missing'>): string {
  switch (c.status) {
    case 'ok': return 'Ready'
    case 'signed out': return 'Sign in to use it'
    case 'missing secrets': return `Needs ${c.missing.join(', ') || 'a key'}`
    case 'unknown service': return 'Not installed on this Mac any more'
    case 'no such profile': return 'Its saved login is gone'
    case 'not granted': return 'Not connected'
    default: return c.status
  }
}

/** Skills that can come off: only linked ones (the agent's own and learned ones are its package's). */
export const removableSkill = (s: KithAgentSkill) => s.kind === 'linked'

/**
 * The bots a connector or skill can be put on: the owner's kith agents. A group, a guest's bot and a bot with no
 * agent are not targets; one agent that has several chats is offered once.
 */
export function targetBots(bots: Iterable<Bot>): Bot[] {
  const seen = new Set<string>()
  const out: Bot[] = []
  for (const b of bots) {
    if (!b.hqAgent || b.kind === 'group' || b.botGuest || seen.has(b.hqAgent)) continue
    seen.add(b.hqAgent)
    out.push(b)
  }
  return out.sort((a, b) => a.name.localeCompare(b.name))
}

/** Which agent names hold each skill, from every agent's own list. */
export function skillHolders(toolsByAgent: ReadonlyMap<string, KithTools>): Map<string, string[]> {
  const out = new Map<string, string[]>()
  for (const [agent, t] of toolsByAgent) {
    for (const s of t.skills) if (s.kind === 'linked') out.set(s.name, [...(out.get(s.name) ?? []), agent])
  }
  return out
}

/** A shared bot of `kith market --json`. */
export interface MarketBot {
  repo: string
  name: string
  description: string
  stars: number
  url: string
  installed: boolean
  installedAs: string | null
}

export function parseMarket(raw: unknown): MarketBot[] {
  return rows(raw).map((m) => ({
    repo: str(m.repo),
    name: str(m.name),
    description: str(m.description),
    stars: typeof m.stars === 'number' ? m.stars : 0,
    url: str(m.url),
    installed: m.installed === true,
    installedAs: str(m.installed_as) || null,
  })).filter((m) => m.repo)
}

/** What installing a shared bot would do (`kith install <link> --preview --json`). */
export interface InstallPreview {
  name: string
  nameTaken: boolean
  suggestedName: string
  description: string
  harness: string
  needs: { service: string; known: boolean; hasProfile: boolean; granted: boolean }[]
  requires: { program: string; why: string; present: boolean }[]
  skills: { own: string[]; linked: string[] }
  permissions: { mode: string; allow: string[]; hooks: string[]; mcp: string[]; tools: string[] }
  warnings: string[]
}

export function parsePreview(raw: unknown): InstallPreview {
  const r = (raw && typeof raw === 'object' ? raw : {}) as Record<string, unknown>
  const p = (r.permissions && typeof r.permissions === 'object' ? r.permissions : {}) as Record<string, unknown>
  const sk = (r.skills && typeof r.skills === 'object' ? r.skills : {}) as Record<string, unknown>
  // Own skills are a list of folder names; linked ones and the MCP servers are {name: where}.
  const names = (v: unknown) => (Array.isArray(v) ? strs(v) : v && typeof v === 'object' ? Object.keys(v) : [])
  const withWhere = (v: unknown) => (v && typeof v === 'object' ? Object.entries(v).map(([k, w]) => `${k}: ${typeof w === 'string' ? w : '?'}`) : strs(v))
  return {
    name: str(r.name),
    nameTaken: r.name_taken === true,
    suggestedName: str(r.suggested_name),
    description: str(r.description),
    harness: str(r.harness),
    needs: rows(r.needs).map((n) => ({ service: str(n.service), known: n.known === true, hasProfile: n.has_profile === true, granted: n.granted === true })),
    requires: rows(r.requires).map((q) => ({ program: str(q.program), why: str(q.why), present: q.present === true })),
    skills: { own: names(sk.own), linked: names(sk.linked) },
    permissions: {
      mode: str(p.permission_mode),
      allow: strs(p.allow),
      hooks: strs(p.hooks),
      mcp: withWhere(p.mcp),
      tools: strs(p.tools),
    },
    warnings: strs(r.warnings),
  }
}

/** The `--as` name an install uses: nothing while the preview's name is free, else the one the person typed or the suggestion. */
export function installName(preview: InstallPreview, typed: string): string | null {
  if (!preview.nameTaken) return null
  return typed.trim() || preview.suggestedName || null
}

/** Whether Install may go: a taken name needs a different one. */
export const canInstall = (preview: InstallPreview, typed: string) => !preview.nameTaken || (installName(preview, typed) ?? preview.name) !== preview.name
