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
