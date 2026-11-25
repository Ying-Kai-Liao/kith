import type { GrantedAgent } from '@shared/mac-role'

export type Level = 'Can chat' | 'Can work'

/** The two sharing levels: guest asks and gets answers; member also runs tools and code on the owner's computer. */
export const levelLabel = (role: GrantedAgent['role'] | null | undefined): Level | null =>
  role === 'member' ? 'Can work' : role === 'guest' ? 'Can chat' : null

export const LEVELS_EXPLAINED = "Can chat: ask and get answers. Can work: also runs tools and code on the owner's computer."

export interface SharedAgentRow {
  key: string
  name: string
  level: Level | null
}

/** Rows for the agents given to this account, "Can work" first, then by name. Level is null when the host gave no role. */
export function sharedAgentRows(granted: readonly { agent: string; name: string | null; role?: GrantedAgent['role'] }[]): SharedAgentRow[] {
  return granted
    .map((g) => ({ key: g.agent, name: g.name ?? g.agent, level: levelLabel(g.role) }))
    .sort((a, b) => Number(b.level === 'Can work') - Number(a.level === 'Can work') || a.name.localeCompare(b.name))
}

/** What the client can't read from its store yet (rooms shared with this account). */
export interface SharedWithYouProvider {
  rooms(computerId: string): string[]
}

// backend later: the real provider reads rooms shared with this account from the host.
export const mockSharedWithYou: SharedWithYouProvider = { rooms: () => [] }
