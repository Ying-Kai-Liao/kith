/** People the owner invited (host methods `people`, `invites`, `audit`; serve/host/src/api/people.rs). */

export type Role = 'member' | 'guest'
/** What the connected Mac says this account is: from the cloud's computer rows. */
export type ComputerRole = 'owner' | 'member'

export interface PersonDevice {
  key: string
  name: string | null
  platform: string | null
  lastSeenAt: number | null
  connected: boolean
}

export interface Person {
  personId: string
  name: string
  agents: Record<string, Role>
  projects: string[]
  joined: boolean
  openInvites: number
  devices: PersonDevice[]
  /** Bots this person invites start without waiting for the owner. */
  autoBots: boolean
  /** The owner revoked them; Restore access undoes it. False from a host that doesn't say. */
  revoked: boolean
}

export interface PendingInvite {
  inviteId: string
  personId: string
  agents: Record<string, Role>
  createdAt: number
  expiresAt: number
  usedAt: number | null
  revokedAt: number | null
}

export interface CreatedInvite {
  inviteId: string
  link: string
  expiresAt: number
  personId: string
}

export interface AuditRow {
  id: number
  at: number
  personId: string
  method: string
  target: string | null
  allowed: boolean
}

const rec = (v: unknown): Record<string, unknown> => (v && typeof v === 'object' ? (v as Record<string, unknown>) : {})
const list = (v: unknown): unknown[] => (Array.isArray(v) ? v : [])
const str = (v: unknown): string => (typeof v === 'string' ? v : '')
const num = (v: unknown): number | null => (typeof v === 'number' ? v : null)

function roles(v: unknown): Record<string, Role> {
  const out: Record<string, Role> = {}
  for (const [agent, role] of Object.entries(rec(v))) out[agent] = role === 'member' ? 'member' : 'guest'
  return out
}

export function parsePeople(raw: unknown): Person[] {
  return list(rec(raw).people).map((p) => {
    const r = rec(p)
    return {
      personId: str(r.personId),
      name: str(r.name) || str(r.personId),
      agents: roles(r.agents),
      projects: list(r.projects).map(str),
      joined: r.joined === true,
      openInvites: num(r.openInvites) ?? 0,
      autoBots: r.autoBots === true,
      revoked: r.revoked === true,
      devices: list(r.devices).map((d) => {
        const x = rec(d)
        return { key: str(x.key), name: str(x.name) || null, platform: str(x.platform) || null, lastSeenAt: num(x.lastSeenAt), connected: x.connected === true }
      }),
    }
  })
}

export function parseInvites(raw: unknown): PendingInvite[] {
  return list(rec(raw).invites).map((i) => {
    const r = rec(i)
    return {
      inviteId: str(r.inviteId),
      personId: str(r.personId),
      agents: roles(r.agents),
      createdAt: num(r.createdAt) ?? 0,
      expiresAt: num(r.expiresAt) ?? 0,
      usedAt: num(r.usedAt),
      revokedAt: num(r.revokedAt),
    }
  })
}

/** Invites someone can still accept. */
export const isOpenInvite = (i: PendingInvite, now: number) => !i.usedAt && !i.revokedAt && i.expiresAt > now

export function parseCreatedInvite(raw: unknown): CreatedInvite {
  const r = rec(raw)
  return { inviteId: str(r.inviteId), link: str(r.link), expiresAt: num(r.expiresAt) ?? 0, personId: str(r.personId) }
}

export function parseAudit(raw: unknown): AuditRow[] {
  return list(rec(raw).audit).map((a) => {
    const r = rec(a)
    return { id: num(r.id) ?? 0, at: num(r.at) ?? 0, personId: str(r.personId), method: str(r.method), target: str(r.target) || null, allowed: r.allowed === true }
  })
}

/** Settings pages by role. Someone invited to a Mac sees only what is theirs: no computers, devices, connectors, voice keys or updates. */
export function pagesFor<T extends string>(role: ComputerRole, all: readonly T[], ownerOnly: readonly T[]): T[] {
  return role === 'owner' ? [...all] : all.filter((p) => !ownerOnly.includes(p))
}
