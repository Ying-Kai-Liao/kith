import assert from 'node:assert/strict'
import { test } from 'node:test'
import { agentsWithout, invitesOnly, inviteAgents, openInvitesFor, sharedWith } from './agent-share.ts'
import type { PendingInvite, Person } from './people.ts'

const person = (personId: string, agents: Person['agents']): Person => ({ personId, name: personId, agents, projects: ['p'], joined: true, openInvites: 0, devices: [], autoBots: false, revoked: false })
const invite = (inviteId: string, agents: PendingInvite['agents'], over: Partial<PendingInvite> = {}): PendingInvite =>
  ({ inviteId, personId: 'a', agents, createdAt: 0, expiresAt: 2000, usedAt: null, revokedAt: null, ...over })

test('sharedWith lists only people holding the agent, with the role', () => {
  const rows = sharedWith([person('a', { house: 'member', coder: 'guest' }), person('b', { coder: 'guest' }), person('c', {})], 'house')
  assert.deepEqual(rows.map((r) => [r.person.personId, r.role]), [['a', 'member']])
})

test('openInvitesFor skips used, revoked, expired and other agents', () => {
  const list = [
    invite('ok', { house: 'guest' }),
    invite('used', { house: 'guest' }, { usedAt: 5 }),
    invite('revoked', { house: 'guest' }, { revokedAt: 5 }),
    invite('expired', { house: 'guest' }, { expiresAt: 500 }),
    invite('other', { coder: 'guest' }),
  ]
  assert.deepEqual(openInvitesFor(list, 'house', 1000).map((i) => i.inviteId), ['ok'])
})

test('inviteAgents names exactly one agent', () => {
  assert.deepEqual(inviteAgents('house', 'guest'), { house: 'guest' })
})

test('agentsWithout drops only this agent and leaves the rest and the person untouched', () => {
  const p = person('a', { house: 'member', coder: 'guest' })
  assert.deepEqual(agentsWithout(p, 'house'), { coder: 'guest' })
  assert.deepEqual(p.agents, { house: 'member', coder: 'guest' })
  assert.deepEqual(agentsWithout(person('b', { house: 'guest' }), 'house'), {})
})

test('invitesOnly drops invites of people already listed as holders', () => {
  const holders = sharedWith([person('a', { house: 'guest' })], 'house')
  const open = [invite('1', { house: 'guest' }, { personId: 'a' }), invite('2', { house: 'guest' }, { personId: 'b' })]
  assert.deepEqual(invitesOnly(open, holders).map((i) => i.inviteId), ['2'])
})
