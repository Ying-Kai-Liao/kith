import type { SeedBot, SeedEntry } from './story'

// `?people`: a room with people (screenshots of Room info's People section, the add sheet, the join line, the hidden
// marker, the won't-answer line and the waiting pointer). Studio holds Pacer and Coder; Ben is in it, Ann lost Coder,
// Mia and Leo can be added, Cy's invite is waiting.

export const coder: SeedBot = { id: 'coder', hqAgent: 'coder', name: 'Coder', description: 'Writes the code', avatarColor: 'kith:violet', avatarShape: 'blob', backend: 'claude', model: 'sonnet' }
export const studio: SeedBot = { id: 'studio', kind: 'group', members: ['pacer', 'coder'], name: 'Studio', description: 'Launch plan', avatarColor: '', avatarShape: 'blob', backend: '', model: null }

const person = (personId: string, name: string, agents: Record<string, string>, joined = true) => ({
  personId, name, agents, everyoneAgents: {}, projects: [], joined, openInvites: 0, devices: joined ? [{ key: personId, name: 'Laptop', platform: 'macos', lastSeenAt: Date.now() - 2 * 3_600_000, connected: false }] : [], autoBots: false, revoked: false,
})

export const people = [
  person('ben', 'Ben', { pacer: 'guest', coder: 'guest' }),
  person('ann', 'Ann', { pacer: 'guest' }),
]

/** Entries of the Studio room, in order; `join` is where Ben's stay opened (its seq is the stay's gen). */
export const entries: SeedEntry[] = [
  { id: 'st1', botId: 'studio', kind: 'user', at: 40, data: { text: 'Can you sketch the launch plan?', status: 'sent' } },
  { id: 'st2', botId: 'studio', kind: 'agent', at: 38, data: { text: 'Three steps: copy, mobile pass, announce.', final: true, author: 'pacer' } },
  { id: 'st-join', botId: 'studio', kind: 'notice', at: 20, data: { notice: 'personJoined', person: { id: 'ben', name: 'Ben' }, style: 'info', text: 'Ben joined · sees this room from now on' } },
  { id: 'st3', botId: 'studio', kind: 'agent', at: 18, data: { text: 'Copy is two lines; mobile gets a static image.', final: true, author: 'pacer', hiddenFrom: ['ben'] } },
  { id: 'st-join2', botId: 'studio', kind: 'notice', at: 12, data: { notice: 'personJoined', person: { id: 'ann', name: 'Ann' }, style: 'info', text: 'Ann joined · sees this room from now on' } },
  { id: 'st4', botId: 'studio', kind: 'user', at: 8, data: { text: '@Pacer can you shrink the scope?', status: 'sent' } },
  { id: 'st5', botId: 'studio', kind: 'agent', at: 7, data: { text: "Pacer didn't answer: Ann joined the room first. Ask again.", final: true, author: 'pacer', unanswered: true } },
  { id: 'st6', botId: 'studio', kind: 'notice', at: 3, data: { text: 'Pacer is waiting for the owner to answer a card.', style: 'info' } },
  {
    id: 'st-card', botId: 'pacer', kind: 'permission', at: 3,
    data: {
      title: 'Edit launch.md', toolKind: 'edit', status: 'pending', author: 'pacer', room: 'studio', personName: 'everyone in Studio',
      options: [{ optionId: 'allow', name: 'Allow once', kind: 'allow_once' }, { optionId: 'deny', name: 'Deny', kind: 'reject_once' }],
    },
  },
]

/** Stays by person, as `roomPeople` returns them once the entries above are added (gens are filled in from their seqs). */
export const invites = [
  { inviteId: 'inv-cy', personId: 'cy', agents: { pacer: 'guest', coder: 'guest' }, createdAt: Date.now() - 3_600_000, expiresAt: Date.now() + 80_000_000, usedAt: null, revokedAt: null, rooms: [{ room: 'studio', done: false }] },
]
export const extraPeople = [person('cy', 'Cy', { pacer: 'guest', coder: 'guest' }, false)]
