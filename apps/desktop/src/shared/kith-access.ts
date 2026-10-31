import { ago, type AccessGrant } from './kith-info.ts'
import type { KeyRow } from './secret-card.ts'

/** The owner's service accounts and keys this agent holds (`listKeys` rows whose `agents` include it). */
export const agentKeys = (keys: KeyRow[], agent: string): KeyRow[] => keys.filter((k) => k.agents.includes(agent))

/** "notion · library": the account row's text. */
export const accountText = (k: Pick<KeyRow, 'service' | 'profile' | 'account'>): string => `${k.service} · ${k.account || k.profile}`

/** A key's name: the variables it sets, else its account. */
export const keyName = (k: Pick<KeyRow, 'vars' | 'account'>): string => k.vars.join(', ') || k.account

/** "Keychain · last used 2 h ago", or "Keychain · never used". */
export function keyDetail(k: Pick<KeyRow, 'lastUsed'>, now = Date.now()): string {
  const at = k.lastUsed ? Date.parse(k.lastUsed) : NaN
  return `Keychain · ${Number.isNaN(at) ? 'never used' : `last used ${ago(at, now)}`}`
}

export interface PersonGrants {
  person: string
  name: string
  grants: AccessGrant[]
}

/** The agent's grants by person, in the order each person first appears. */
export function groupGrants(grants: AccessGrant[]): PersonGrants[] {
  const by = new Map<string, PersonGrants>()
  for (const g of grants) {
    const hit = by.get(g.person)
    if (hit) {
      hit.grants.push(g)
      if (!hit.name && g.name) hit.name = g.name
    } else by.set(g.person, { person: g.person, name: g.name, grants: [g] })
  }
  return [...by.values()]
}

/** A person's heading: their name, "Everyone" for the `*` row, else their id. */
export const personTitle = (p: Pick<PersonGrants, 'person' | 'name'>): string => (p.person === '*' ? 'Everyone' : p.name || p.person)

/** A host's refusal for a member (403) or its lack of the method (404): the section stays hidden. */
export const accessHidden = (error: unknown): boolean => {
  const status = (error as { status?: number } | null)?.status
  return status === 403 || status === 404 || /unknown method/i.test((error as { message?: string } | null)?.message ?? '')
}

/** One thing "Always allow" remembered for an agent in auto mode (`allowedRules`). */
export interface AllowedRule {
  id: string
  rule: string
  kind: 'rule' | 'prose'
  reason: string
  added: string | null
}

export interface AllowedList {
  rules: AllowedRule[]
  /** Set when the file could not be read whole; the rules that could be read are still in `rules`. */
  error: string | null
}

/** `allowedRules`' answer as rows; anything that is not a rule with an id is dropped. */
export function parseAllowed(raw: unknown): AllowedList {
  const o = (raw ?? {}) as { allowed?: unknown; error?: unknown }
  const list = Array.isArray(o.allowed) ? o.allowed : []
  const rules = list.flatMap((r): AllowedRule[] => {
    const e = r as Record<string, unknown>
    if (typeof e?.id !== 'string' || typeof e.rule !== 'string' || !e.rule) return []
    return [{
      id: e.id,
      rule: e.rule,
      kind: e.kind === 'prose' ? 'prose' : 'rule',
      reason: typeof e.reason === 'string' ? e.reason : '',
      added: typeof e.added === 'string' && !Number.isNaN(Date.parse(e.added)) ? e.added : null,
    }]
  })
  return { rules, error: typeof o.error === 'string' && o.error ? o.error : null }
}

/** A permission rule as it is; a sentence for the classifier cut to one short line. */
export function ruleText(r: Pick<AllowedRule, 'rule' | 'kind'>, max = 90): string {
  if (r.kind === 'rule' || r.rule.length <= max) return r.rule
  return `${r.rule.slice(0, max - 1).trimEnd()}…`
}

/** "Blocked: [Data Exfiltration] · 2 h ago": why it was blocked, and when it was remembered. */
export function ruleDetail(r: Pick<AllowedRule, 'reason' | 'added'>, now = Date.now()): string {
  const parts = [r.reason ? `Blocked: ${r.reason}` : '', r.added ? ago(Date.parse(r.added), now) : '']
  return parts.filter(Boolean).join(' · ')
}
