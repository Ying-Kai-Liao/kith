// The Access and Key cards: a permission entry whose `data.card` is "account" or "key" (kernel/PROTOCOL.md, "Keys").
// The entry never holds a value. The one place a value lives is the Key card's own state while it is being typed.

import type { Entry } from './models.ts'

export interface SecretRequest {
  service: string
  /** The variable names the key has, one field each. */
  vars: string[]
  /** The profile (`github/work`) the key goes to or replaces; null while the owner has not named it. */
  profile: string | null
  account: string | null
  purpose: string
  /** The card replaces an existing key: the profile is fixed. */
  replaces: boolean
  state: 'pending' | 'saved' | 'denied' | 'expired'
}

export interface SecretAccount {
  profile: string
  account: string
  agents: string[]
}

/** What `listKeys` returns per key; never a value. */
export interface KeyRow {
  profile: string
  service: string
  account: string
  owner: string | null
  vars: string[]
  agents: string[]
  lastUsed: string | null
}

/** Server limits (kith-auth): each value 1..1536 bytes, no NUL. */
export const MAX_VALUE_BYTES = 1536
export const ACCOUNT_PATTERN = /^[a-z0-9][a-z0-9._-]{0,39}$/

export const isAccountName = (v: string) => ACCOUNT_PATTERN.test(v)

/** Why a value can't be sent, as the card words it; null when it can. */
export function valueProblem(v: string): string | null {
  if (v.length === 0) return 'Enter every field of the key'
  if (v.includes('\0')) return "That key can't be used"
  if (new TextEncoder().encode(v).length > MAX_VALUE_BYTES) return 'That key is too long'
  return null
}

export const isSecretCard = (e: Pick<Entry, 'kind' | 'data'>) => e.kind === 'permission' && (e.data.card === 'account' || e.data.card === 'key') && !!e.data.secret

/** The card's state word once it is closed. */
export function secretOutcome(secret: SecretRequest): string | null {
  switch (secret.state) {
    case 'saved':
      return `Saved as ${secret.profile ?? secret.service}`
    case 'denied':
      return 'Not now'
    case 'expired':
      return 'Expired'
    default:
      return null
  }
}

/** The line the island, the board and notifications show: never a field. */
export const secretSummary = (agent: string, service: string) => `${agent} needs a key for ${service}. Open to add it.`

export function parseKeys(raw: unknown): KeyRow[] {
  const list = (raw as { keys?: unknown } | null)?.keys
  if (!Array.isArray(list)) return []
  return list.flatMap((k): KeyRow[] => {
    const o = k as Record<string, unknown>
    if (typeof o?.profile !== 'string' || typeof o.service !== 'string') return []
    return [{
      profile: o.profile,
      service: o.service,
      account: typeof o.account === 'string' ? o.account : o.profile.split('/').slice(1).join('/'),
      owner: typeof o.owner === 'string' ? o.owner : null,
      vars: Array.isArray(o.vars) ? o.vars.filter((v): v is string => typeof v === 'string') : [],
      agents: Array.isArray(o.agents) ? o.agents.filter((v): v is string => typeof v === 'string') : [],
      lastUsed: typeof o.last_used === 'string' ? o.last_used : null,
    }]
  })
}

/** Keys grouped by service, services and accounts in name order. */
export function keysByService(keys: KeyRow[]): { service: string; keys: KeyRow[] }[] {
  const groups = new Map<string, KeyRow[]>()
  for (const k of keys) groups.set(k.service, [...(groups.get(k.service) ?? []), k])
  return [...groups].sort(([a], [b]) => a.localeCompare(b)).map(([service, list]) => ({ service, keys: list.sort((a, b) => a.account.localeCompare(b.account)) }))
}
