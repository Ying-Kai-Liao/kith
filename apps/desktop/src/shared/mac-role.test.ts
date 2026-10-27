import assert from 'node:assert/strict'
import { test } from 'node:test'
import { inviteProof } from './invite.ts'
import { runJoin, type JoinState } from './invite-join.ts'
import { agentsWithoutChat, guestPrompts, isAccessRemoved, macRole, scopedKey, ownerNameFrom, parseGranted, visibleBots, waitingFor } from './mac-role.ts'
import type { Entry } from './models.ts'
import { relayJoinDeps, type RelayApi } from './relay-access.ts'
import type { RelayComputer } from './relay.ts'

const g = (agent: string, role: 'member' | 'guest') => ({ agent, name: null, role })

test('role per Mac: the host list wins, then the cloud row or a kept token, else owner', () => {
  assert.equal(macRole({ granted: [g('a', 'guest'), g('b', 'member')] }), 'member')
  assert.equal(macRole({ granted: [g('a', 'guest')], cloudRole: 'owner' }), 'guest')
  assert.equal(macRole({ granted: [] }), 'guest')
  assert.equal(macRole({ granted: null, cloudRole: 'member' }), 'member')
  assert.equal(macRole({ granted: null, hasToken: true }), 'member')
  assert.equal(macRole({ granted: null }), 'owner')
  // Being a member on Mac B says nothing about Mac A.
  assert.equal(macRole({ granted: null, cloudRole: 'owner' }), 'owner')
})

test('parseGranted drops rows it cannot read and says null when absent', () => {
  assert.equal(parseGranted(undefined), null)
  assert.deepEqual(parseGranted([{ agent: 'house', name: 'House', role: 'member' }, { nope: 1 }, { agent: 'x', role: 'weird' }]), [
    { agent: 'house', name: 'House', role: 'member' },
    { agent: 'x', name: null, role: 'guest' },
  ])
})

test('the roster keeps only bots of granted agents, and everything on an owner Mac', () => {
  const bots = [{ id: '1', hqAgent: 'house' }, { id: '2', hqAgent: 'finance' }, { id: '3', hqAgent: null }]
  assert.deepEqual(visibleBots(bots, [g('house', 'member')]).map((b) => b.id), ['1'])
  assert.deepEqual(visibleBots(bots, null).map((b) => b.id), ['1', '2', '3'])
  assert.deepEqual(agentsWithoutChat(bots, [g('house', 'member'), g('video', 'guest')]).map((a) => a.agent), ['video'])
})

const card = (id: string, bot: string, data: Entry['data'], at = 1): Entry => ({ id, seq: at, botId: bot, rev: 1, kind: 'permission', turn: 1, data, createdAt: at, updatedAt: at })
const opts = [{ optionId: 'ok', name: 'Allow once', kind: 'allow_once' }]

test('guest prompts: pending, for the owner, from bots outside the roster', () => {
  const entries = new Map<string, Entry[]>([
    ['mine', [card('c0', 'mine', { status: 'pending', title: 'own' })]],
    ['guestbot', [
      card('c1', 'guestbot', { status: 'pending', approver: 'owner', personName: 'Mia', agent: 'house', title: 'rm -rf x', options: opts }, 5),
      card('c2', 'guestbot', { status: 'answered', approver: 'owner', personName: 'Mia', agent: 'house' }, 6),
      card('c3', 'guestbot', { status: 'pending', approver: 'member', personName: 'Mia', agent: 'house' }, 7),
    ]],
    ['other', [card('c4', 'other', { status: 'pending', approver: 'owner', person: 'lee', agent: 'video', title: 'ls', options: opts }, 2)]],
  ])
  const list = guestPrompts(entries, new Map([['mine', 1]]))
  assert.deepEqual(list.map((p) => [p.entryId, p.personName, p.agent, p.options.length]), [['c4', 'lee', 'video', 1], ['c1', 'Mia', 'house', 1]])
  // Answered elsewhere: the entry updates and drops out.
  entries.get('guestbot')![0] = card('c1', 'guestbot', { status: 'answered', approver: 'owner', personName: 'Mia' }, 5)
  assert.deepEqual(guestPrompts(entries, new Map([['mine', 1]])).map((p) => p.entryId), ['c4'])
})

test("the owner's own device shows the buttons on a card addressed to the owner", () => {
  // A -p run's card (kith ask, a schedule) and a bot guest's card both carry approver 'owner'.
  assert.equal(waitingFor(card('a', 'b', { status: 'pending', approver: 'owner' }), 'Kai', true), null)
  assert.equal(waitingFor(card('a', 'b', { status: 'pending', approver: 'owner' }), null, true), null)
  assert.equal(waitingFor(card('a', 'b', { status: 'pending', approver: 'member' }), null, true), null)
})

test('an invitee sees "Waiting for <name>" on an owner-approved card only while pending', () => {
  assert.equal(waitingFor(card('a', 'b', { status: 'pending', approver: 'owner' }), 'Kai', false), 'Kai')
  assert.equal(waitingFor(card('a', 'b', { status: 'pending', approver: 'owner' }), null, false), 'the owner')
  assert.equal(waitingFor(card('a', 'b', { status: 'pending', approver: 'member' }), 'Kai', false), null)
  assert.equal(waitingFor(card('a', 'b', { status: 'answered', approver: 'owner' }), 'Kai', false), null)
  assert.equal(ownerNameFrom('Waiting for Kai'), 'Kai')
  assert.equal(ownerNameFrom('Needs approval: x'), null)
})

test('the join over the relay: token kept, then access, then proveInvite as the first host call', async () => {
  const log: string[] = []
  const computer: RelayComputer = { computerId: 'cmp_1', name: 'Mac', role: 'member', platform: 'macos', signKey: 'k', online: true, access: 'none' }
  const api: RelayApi = {
    computers: async () => ({ ok: true, value: [computer] }),
    requestAccess: async () => (log.push('request'), { ok: true, value: { requestId: 'r1', expiresAt: 10 ** 13, code: '123456', computerId: 'cmp_1' } }),
    accessStatus: async () => ({ ok: true, value: 'approved' }),
    cancelAccess: async () => {},
    state: async () => ({ kind: 'ready' }),
    call: async (_c, _k, method) => (log.push(`host:${method}`), { proven: true }),
  }
  const link = { computerId: 'cmp_1', inviteId: 'inv_1', token: 'x'.repeat(24) }
  const accepted: string[] = []
  const deps = relayJoinDeps(api, {
    signedIn: () => true,
    waitForSignIn: async () => {},
    accept: async (_i, h1) => (accepted.push(h1), { computerId: 'cmp_1' }),
    keepToken: async () => void log.push('keep'),
  })
  const steps: JoinState[] = []
  const end = await runJoin(link, deps, (s) => steps.push(s))
  assert.equal(end.step, 'done')
  assert.deepEqual(log, ['keep', 'request', 'host:proveInvite'])
  assert.deepEqual(accepted, [await inviteProof(link.token)])
  // A member's device shows its digits too: in Code mode the Mac's owner compares them before approving.
  assert.deepEqual(steps.filter((s) => s.code !== null).map((s) => [s.step, s.code]), [['asking', '123456']])
})

test('a device the cloud lists as granted joins without asking again', async () => {
  const log: string[] = []
  const api = {
    computers: async () => ({ ok: true, value: [{ computerId: 'cmp_1', name: 'M', role: 'member', platform: '', signKey: 'k', online: true, access: 'granted' }] }),
    requestAccess: async () => (log.push('request'), { ok: false, error: { status: 409, code: 'conflict', message: 'This device already has access' } }),
    state: async () => ({ kind: 'ready' }),
    call: async (_c: string, _k: string, method: string) => (log.push(`host:${method}`), { proven: true }),
  } as unknown as RelayApi
  const deps = relayJoinDeps(api, { signedIn: () => true, waitForSignIn: async () => {}, accept: async () => ({ computerId: 'cmp_1' }), keepToken: async () => {} })
  const end = await runJoin({ computerId: 'cmp_1', inviteId: 'i', token: 'y'.repeat(24) }, deps, () => {})
  assert.equal(end.step, 'done')
  assert.deepEqual(log, ['host:proveInvite'])
})

test('asking again when the cloud says this device already has access counts as approved', async () => {
  const log: string[] = []
  const api = {
    // The list was read before the grant landed: it still says none, and the ask meets the grant.
    computers: async () => ({ ok: true, value: [{ computerId: 'cmp_1', name: 'M', role: 'member', platform: '', signKey: 'k', online: true, access: 'none' }] }),
    requestAccess: async () => (log.push('request'), { ok: false, error: { status: 409, code: 'conflict', message: 'This device already has access' } }),
    state: async () => ({ kind: 'ready' }),
    call: async (_c: string, _k: string, method: string) => (log.push(`host:${method}`), { proven: true }),
  } as unknown as RelayApi
  const deps = relayJoinDeps(api, { signedIn: () => true, waitForSignIn: async () => {}, accept: async () => ({ computerId: 'cmp_1' }), keepToken: async () => {} })
  const end = await runJoin({ computerId: 'cmp_1', inviteId: 'i', token: 'y'.repeat(24) }, deps, () => {})
  assert.equal(end.step, 'done')
  assert.deepEqual(log, ['request', 'host:proveInvite'])
})

test('other failures to ask still end the join', async () => {
  const api = {
    computers: async () => ({ ok: true, value: [{ computerId: 'cmp_1', name: 'M', role: 'member', platform: '', signKey: 'k', online: true, access: 'none' }] }),
    requestAccess: async () => ({ ok: false, error: { status: 429, code: 'rateLimited', message: 'Too many requests' } }),
  } as unknown as RelayApi
  const deps = relayJoinDeps(api, { signedIn: () => true, waitForSignIn: async () => {}, accept: async () => ({ computerId: 'cmp_1' }), keepToken: async () => {} })
  const end = await runJoin({ computerId: 'cmp_1', inviteId: 'i', token: 'y'.repeat(24) }, deps, () => {})
  assert.equal(end.step, 'failed')
  assert.equal(end.message, 'Too many requests')
})

test('an offline Mac waits instead of failing', async () => {
  const api = {
    computers: async () => ({ ok: true, value: [{ computerId: 'cmp_1', name: 'M', role: 'member', platform: '', signKey: 'k', online: false, access: 'granted' }] }),
  } as unknown as RelayApi
  const deps = relayJoinDeps(api, { signedIn: () => true, waitForSignIn: async () => {}, accept: async () => ({ computerId: 'cmp_1' }), keepToken: async () => {} })
  const end = await runJoin({ computerId: 'cmp_1', inviteId: 'i', token: 'y'.repeat(24) }, deps, () => {})
  assert.equal(end.step, 'waiting')
})

test('only the link\'s access-removed 401 counts as removed; a lease that may come back keeps retrying', () => {
  assert.equal(isAccessRemoved(401, 'accessRemoved'), true)
  assert.equal(isAccessRemoved(401, 'leaseExpired'), false)
  assert.equal(isAccessRemoved(401, undefined), false)
  assert.equal(isAccessRemoved(0, 'accessRemoved'), false)
})

test('what is kept about a Mac is keyed by account as well as computer', () => {
  assert.notEqual(scopedKey('user_a', 'cmp_1'), scopedKey('user_b', 'cmp_1'))
  assert.notEqual(scopedKey('user_a', 'cmp_1'), scopedKey('user_a', 'cmp_2'))
  assert.equal(scopedKey('user_a', 'cmp_1'), scopedKey('user_a', 'cmp_1'))
})
