// People in a group room (docs: designer SPEC "Rooms with people"): who can open the room, what Kai has to fix, the
// picker's groups, the hidden-message marker and the invite sheet's room switches. Pure; no host calls here.
import type { MyRoom, RoomPerson } from './host-contract.gen.ts'
import type { PendingInvite, Person, Role } from './people.ts'

/** A room's agent: the Kith agent behind one of its member bots (`id` is the agent name roles are keyed by). */
export interface RoomAgent {
  id: string
  name: string
}

/** The ways a group can fail to take people: an outside bot (or a member with no agent behind it) is in it, or no agent is. */
export type RoomBlock = { kind: 'outsideBot'; name: string } | { kind: 'noAgents' }

export interface RoomMemberFacts {
  id: string
  name: string
  hqAgent?: string | null
  /** Set for an outside bot's own chat; an outside bot in a room is a member id with no bot row at all. */
  botGuest?: unknown
}

/** Members of a room, split into its Kith agents and whatever is outside (an id the roster has no chat for, a bot guest). */
export function roomParts(members: readonly string[], find: (id: string) => RoomMemberFacts | undefined, outsideName: (id: string) => string): { agents: RoomAgent[]; outside: { id: string; name: string }[] } {
  const agents: RoomAgent[] = []
  const outside: { id: string; name: string }[] = []
  for (const id of members) {
    const bot = find(id)
    if (bot?.hqAgent && !bot.botGuest) {
      if (!agents.some((a) => a.id === bot.hqAgent)) agents.push({ id: bot.hqAgent, name: bot.name })
    } else outside.push({ id, name: bot?.name ?? outsideName(id) })
  }
  return { agents, outside }
}

export function roomBlock(parts: { agents: RoomAgent[]; outside: { name: string }[] }): RoomBlock | null {
  if (parts.outside.length) return { kind: 'outsideBot', name: parts.outside[0]!.name }
  return parts.agents.length ? null : { kind: 'noAgents' }
}

export const blockText = (block: RoomBlock, room: string) =>
  block.kind === 'outsideBot' ? `People can't join a room with an outside bot. Remove ${block.name} from ${room} first.` : `Add an agent to ${room} first.`

/** True when the person holds the agent on their own row or through everyone's. */
export const holds = (person: Person, agent: string) => agent in person.agents || agent in person.everyoneAgents

/** The room's agents this person lacks, in the room's order. */
export const missingFor = (person: Person, agents: readonly RoomAgent[]): RoomAgent[] => agents.filter((a) => !holds(person, a.id))

const join = (names: string[]) => (names.length <= 1 ? (names[0] ?? '') : names.length === 2 ? `${names[0]} and ${names[1]}` : `${names.slice(0, -1).join(', ')} and ${names.at(-1)}`)
export { join as joinNames }

/** "coder", "coder or video", else "3 of the agents here" (the needs-you detail). */
export function lackLine(missing: readonly RoomAgent[]): string {
  if (missing.length >= 3) return `Doesn't have ${missing.length} of the agents here`
  return `Doesn't have ${missing.map((a) => a.name).join(' or ')} any more`
}

export const openStay = (stays: readonly RoomPerson[], person: string): RoomPerson | undefined => stays.find((s) => s.person === person && (s.leftGen ?? null) === null)

export interface NeedsKaiRow {
  personId: string
  name: string
  kind: 'missing' | 'revoked'
  missing: RoomAgent[]
}

/** Open stays that can't open the room right now (a lost grant, or a revoked person): the decisions on top of the list. */
export function needsKai(stays: readonly RoomPerson[], people: readonly Person[], agents: readonly RoomAgent[]): NeedsKaiRow[] {
  const rows: NeedsKaiRow[] = []
  for (const stay of stays) {
    if ((stay.leftGen ?? null) !== null) continue
    const person = people.find((p) => p.personId === stay.person)
    if (!person) continue
    if (person.revoked) rows.push({ personId: person.personId, name: person.name, kind: 'revoked', missing: [] })
    else {
      const missing = missingFor(person, agents)
      if (missing.length) rows.push({ personId: person.personId, name: person.name, kind: 'missing', missing })
    }
  }
  return rows
}

/** People with an open stay who can open the room (not in `needsKai`). */
export function inTheRoom(stays: readonly RoomPerson[], people: readonly Person[], agents: readonly RoomAgent[]): Person[] {
  const stuck = new Set(needsKai(stays, people, agents).map((r) => r.personId))
  return stays.flatMap((s) => {
    const person = people.find((p) => p.personId === s.person)
    return person && (s.leftGen ?? null) === null && !stuck.has(person.personId) ? [person] : []
  })
}

/** People whose every stay here is closed (left or removed): collapsed under "Earlier". */
export function earlier(stays: readonly RoomPerson[], people: readonly Person[]): Person[] {
  const seen = new Set<string>()
  const out: Person[] = []
  for (const s of stays) {
    if (seen.has(s.person)) continue
    seen.add(s.person)
    if (openStay(stays, s.person)) continue
    const person = people.find((p) => p.personId === s.person)
    if (person && !person.revoked) out.push(person)
  }
  return out
}

export interface InvitedRow {
  invite: PendingInvite
  person: Person | undefined
  name: string
  /** `waiting`: not accepted yet; `accepted`: accepted and about to join (or missing an agent); `ranOut`: expired or cancelled before it was done. */
  state: 'waiting' | 'accepted' | 'ranOut'
  missing: RoomAgent[]
}

/** Invites whose part for this room isn't done: one row per person (their newest invite). */
export function invitedRows(invites: readonly PendingInvite[], people: readonly Person[], room: string, agents: readonly RoomAgent[], now: number): InvitedRow[] {
  const rows = new Map<string, InvitedRow>()
  const sorted = [...invites].sort((a, b) => b.createdAt - a.createdAt)
  for (const invite of sorted) {
    const part = invite.rooms?.find((r) => r.room === room)
    if (!part || part.done || rows.has(invite.personId)) continue
    const person = people.find((p) => p.personId === invite.personId)
    const state = invite.revokedAt || (!invite.usedAt && invite.expiresAt <= now) ? 'ranOut' : invite.usedAt ? 'accepted' : 'waiting'
    rows.set(invite.personId, { invite, person, name: person?.name ?? invite.personId, state, missing: person ? missingFor(person, agents) : [] })
  }
  return [...rows.values()]
}

export interface PickerRow {
  person: Person
  missing: RoomAgent[]
  /** Hasn't accepted the invite yet: the add waits for it. */
  accepted: boolean
}

/** The add-person picker: invited people who aren't revoked and aren't in the room now, split by whether they hold every room agent. */
export function pickerGroups(people: readonly Person[], stays: readonly RoomPerson[], agents: readonly RoomAgent[]): { canJoin: PickerRow[]; needsAccess: PickerRow[] } {
  const canJoin: PickerRow[] = []
  const needsAccess: PickerRow[] = []
  for (const person of people) {
    if (person.revoked || openStay(stays, person.personId)) continue
    const missing = missingFor(person, agents)
    const row = { person, missing, accepted: person.joined }
    ;(missing.length ? needsAccess : canJoin).push(row)
  }
  return { canJoin, needsAccess }
}

/** Roles to save to give a person agents: what they hold already, plus these (guest unless picked). */
export function grantRoles(person: Person, given: Record<string, Role>): Record<string, Role> {
  return { ...person.agents, ...given }
}

/** What "won't answer here" says: the agents that stay silent and who is missing access (Kai's line above the box). */
export interface WontAnswer {
  agents: RoomAgent[]
  people: string[]
  revoked: boolean
  line: string
}

export function wontAnswer(rows: readonly NeedsKaiRow[], agents: readonly RoomAgent[]): WontAnswer | null {
  if (!rows.length) return null
  const revokedRows = rows.filter((r) => r.kind === 'revoked')
  const missingRows = rows.filter((r) => r.kind === 'missing')
  // A revoked person closes the room for every agent, whatever else is missing.
  if (revokedRows.length) {
    const names = revokedRows.map((r) => r.name)
    return { agents: [...agents], people: names, revoked: true, line: `No agent answers here while ${join(names)}'s access is revoked.` }
  }
  const silent = agents.filter((a) => missingRows.some((r) => r.missing.some((m) => m.id === a.id)))
  const names = missingRows.map((r) => r.name)
  const who = silent.map((a) => a.name)
  const reason = who.length === 1 && names.length === 1 ? `${names[0]} doesn't have ${who[0]}.` : `${join(names)} ${names.length === 1 ? 'is' : 'are'} missing access.`
  return { agents: silent, people: names, revoked: false, line: `${join(who)} won't answer here. ${reason}` }
}

/** Agent names the @ picker dims in a room, with the reason. */
export const dimmedAgents = (w: WontAnswer | null): Set<string> => new Set(w ? w.agents.map((a) => a.id) : [])

/**
 * The invite sheet's Rooms group: a room is on only while every agent it needs is switched on above. Rooms never switch
 * agents on by themselves; switching a needed agent off turns its room off.
 */
export interface InviteRoomOption {
  id: string
  name: string
  agents: RoomAgent[]
}

export const roomNeeds = (room: InviteRoomOption, roles: Record<string, Role>): RoomAgent[] => room.agents.filter((a) => !(a.id in roles))

export function followAgents(picked: readonly string[], rooms: readonly InviteRoomOption[], roles: Record<string, Role>): string[] {
  return picked.filter((id) => {
    const room = rooms.find((r) => r.id === id)
    return room && roomNeeds(room, roles).length === 0
  })
}

/** A new invite's starting point for a room: its agents on as guest, this room ticked (Add person → Invite someone new). */
export function roomInviteStart(room: InviteRoomOption): { roles: Record<string, Role>; rooms: string[] } {
  return { roles: Object.fromEntries(room.agents.map((a) => [a.id, 'guest' as Role])), rooms: [room.id] }
}

/** "Mia joins Studio when Mia accepts." under the invite link. */
export function inviteJoinsLine(name: string, roomNames: readonly string[]): string | null {
  return roomNames.length ? `${name} joins ${join([...roomNames])} when ${name} accepts.` : null
}

/** Settings > People > edit: rooms a person is in whose agents would no longer all be held after the change. */
export function closingRooms(person: Person, newRoles: Record<string, Role>, inRooms: readonly { room: string; name: string; agents: RoomAgent[] }[]): { room: string; name: string; lost: RoomAgent[] }[] {
  const holdsAfter = (agent: string) => agent in newRoles || agent in person.everyoneAgents
  return inRooms.flatMap((r) => {
    const lost = r.agents.filter((a) => !holdsAfter(a.id))
    return lost.length ? [{ room: r.room, name: r.name, lost }] : []
  })
}

// ---- the chat ----------------------------------------------------------------------------------------------------

/** What an entry carries that the room view reads. */
export interface RoomEntryFacts {
  seq: number
  kind: string
  data: { hiddenFrom?: string[]; notice?: string; person?: { id?: string; name?: string } | string; text?: string; unanswered?: boolean }
}

/**
 * The names a message is marked as "From before <names> joined". Only below that person's join line: a message above it
 * is history the divider already explains. `stays` carry the join gen (its seq).
 */
export function hiddenFromNames(entry: RoomEntryFacts, stays: readonly RoomPerson[], nameOf: (person: string) => string): string[] {
  const hidden = entry.data.hiddenFrom
  if (!hidden?.length) return []
  return hidden.filter((id) => stays.some((s) => s.person === id && entry.seq > s.joinedGen)).map(nameOf)
}

export function hiddenMarkerText(names: readonly string[]): string {
  return names.length > 2 ? `Hidden from ${names.length} people` : `From before ${join([...names])} joined`
}

export function hiddenMarkerHelp(names: readonly string[], agent: string | null): string {
  return agent ? `${agent} started this answer before ${join([...names])} joined, so it was written with your access. Only you see it.` : 'Only you see this.'
}

/** The join divider's words, and the muted second line on the room's first join. */
export function joinLines(entry: RoomEntryFacts, firstJoin: boolean): string[] {
  const person = entry.data.person
  const name = (person && typeof person === 'object' ? person.name : null) ?? 'Someone'
  if (entry.data.notice === 'personLeft') return [`${name} left`]
  return firstJoin ? [`${name} joined · sees from here`, "Agents now answer only when @'d."] : [`${name} joined · sees from here`]
}

export const isPersonNotice = (e: RoomEntryFacts) => e.kind === 'notice' && (e.data.notice === 'personJoined' || e.data.notice === 'personLeft')

/** True when no earlier entry in the list is a join line. */
export const isFirstJoin = (entries: readonly RoomEntryFacts[], entry: RoomEntryFacts) => !entries.some((e) => e.data.notice === 'personJoined' && e.seq < entry.seq)

/** The waiting notice PR C writes ("<agent> is waiting for the owner to answer a card."): the agent's name, else null. */
export function waitingAgent(entry: RoomEntryFacts & { data: { agent?: string } }): string | null {
  if (entry.kind !== 'notice') return null
  if (entry.data.notice === 'roomCardWaiting') return entry.data.agent ?? entry.data.text?.match(/^(.+?) is waiting/)?.[1] ?? null
  return entry.data.text?.match(/^(.+) is waiting for the owner to answer a card\.$/)?.[1] ?? null
}

/** Ben's start-of-history lines and what the room shows him: true when no older page is left to load. */
export const BEN_START_LINES = (room: string) => [`You see ${room} from when Kai added you.`, 'Kai sees everything said here.']

/** The people's side: a room Ben is in, from `myRooms`, or null once it is gone from a refreshed list. */
export const stillIn = (rooms: readonly MyRoom[], id: string): MyRoom | null => rooms.find((r) => r.id === id) ?? null

/** Under Ben's newest message that @'d an agent: "Asked planner" until an agent reply lands after it. */
export function askedLine(entries: readonly { kind: string; seq: number; data: { text?: string; sender?: unknown } }[], agents: readonly string[]): string | null {
  for (let i = entries.length - 1; i >= 0; i--) {
    const e = entries[i]!
    if (e.kind === 'agent') return null
    if (e.kind !== 'user') continue
    const asked = agents.filter((a) => new RegExp(`(^|\\s)@${a.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}(?![\\w-])`, 'i').test(e.data.text ?? ''))
    return asked.length ? `Asked ${join(asked)}` : null
  }
  return null
}
