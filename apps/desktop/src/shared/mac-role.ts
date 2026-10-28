import type { Entry } from './models.ts'
import { decodeAgentRole, type AgentRole } from './host-contract.gen.ts'

/** What this account is on one Mac. Per agent it is member or guest (the owner's kith-auth preset); the Mac shows the best of them. */
export type MacRole = 'owner' | 'member' | 'guest'

/** One agent an invited person may use, from the member `hello` (the host RPC contract's `GrantedAgent`). */
export interface GrantedAgent {
  agent: string
  name: string | null
  role: AgentRole
}

/** The member `hello`'s list; null when absent (an owner's hello, or a host before roles), and unreadable rows are dropped. */
export function parseGranted(raw: unknown): GrantedAgent[] | null {
  if (!Array.isArray(raw)) return null
  const out: GrantedAgent[] = []
  for (const row of raw as Record<string, unknown>[]) {
    if (!row || typeof row.agent !== 'string' || !row.agent) continue
    out.push({ agent: row.agent, name: typeof row.name === 'string' && row.name ? row.name : null, role: decodeAgentRole(row.role) })
  }
  return out
}

/**
 * The role on a Mac. The host's own list wins (the host is the authority); before it answers, the cloud's row or a kept
 * invite token say this is someone else's Mac. Computers saved before roles existed have neither: the owner.
 */
export function macRole(i: { granted: GrantedAgent[] | null; cloudRole?: 'owner' | 'member' | null; hasToken?: boolean }): MacRole {
  if (i.granted) return i.granted.some((g) => g.role === 'member') ? 'member' : 'guest'
  return i.cloudRole === 'member' || i.hasToken ? 'member' : 'owner'
}

export const roleLabel = (r: MacRole) => (r === 'owner' ? 'Owner' : r === 'member' ? 'Member' : 'Guest')

/** Bots shown on an invited person's Mac: only those of agents they hold. An owner's Mac (no list) shows all. */
export function visibleBots<T extends { hqAgent?: string | null }>(bots: T[], granted: GrantedAgent[] | null): T[] {
  if (!granted) return bots
  const allowed = new Set(granted.map((g) => g.agent))
  return bots.filter((b) => !!b.hqAgent && allowed.has(b.hqAgent))
}

/** The granted agents that have no chat yet: what "New chat" offers. */
export function agentsWithoutChat(bots: { hqAgent?: string | null }[], granted: GrantedAgent[] | null): GrantedAgent[] {
  const has = new Set(bots.map((b) => b.hqAgent))
  return (granted ?? []).filter((g) => !has.has(g.agent))
}

/** A guest's permission card waiting for this Mac's owner. */
export interface GuestPrompt {
  entryId: string
  botId: string
  personName: string
  agent: string
  title: string
  options: { optionId: string; name: string; kind: string }[]
  createdAt: number
  entry: Entry
}

/**
 * The cards in an owner's event stream that come from other people's chats: pending, for the owner, and from a bot that
 * isn't in the owner's own roster. They are read from the entries as they are, so one answered anywhere drops out.
 */
export function guestPrompts(entries: ReadonlyMap<string, Entry[]>, ownBots: { has(id: string): boolean }): GuestPrompt[] {
  const out: GuestPrompt[] = []
  for (const [botId, list] of entries) {
    if (ownBots.has(botId)) continue
    for (const e of list) {
      const d = e.data
      if (e.kind !== 'permission' || d.status !== 'pending' || d.approver !== 'owner') continue
      out.push({
        entryId: e.id,
        botId,
        personName: d.personName || d.person || 'Someone',
        agent: d.agent ?? '',
        title: d.title ?? 'Use a tool',
        options: d.options ?? [],
        createdAt: e.createdAt,
        entry: e,
      })
    }
  }
  return out.sort((a, b) => a.createdAt - b.createdAt)
}

/**
 * Whose answer a pending card waits for, or null when it is this account's to answer: the host says so (`canAnswer`,
 * the same rule its guard applies); an older host doesn't, and then a card for the owner is the owner's own on the
 * owner's device.
 */
export function waitingFor(entry: Entry, ownerName: string | null, isOwner: boolean): string | null {
  const d = entry.data
  if (d.status !== 'pending') return null
  if (d.canAnswer !== undefined) return d.canAnswer ? null : ownerName || 'the owner'
  if (isOwner || d.approver !== 'owner') return null
  return ownerName || 'the owner'
}

/** The owner's name from the bot's activity line, which the host words "Waiting for <owner>". */
export function ownerNameFrom(activity: string | null | undefined): string | null {
  const m = /^Waiting for (.+)$/.exec(activity ?? '')
  return m ? m[1]!.trim() : null
}

/** A relayed call or stream ended with this: only the link's own "access removed" counts, not a lease that may come back (also 401). */
export const isAccessRemoved = (status: number, code: string | undefined) => status === 401 && code === 'accessRemoved'

/** Keys for what is kept on this device about a Mac: per account as well as per computer, so another account sees none of it. */
export const scopedKey = (userId: string, computerId: string) => `${userId}/${computerId}`
