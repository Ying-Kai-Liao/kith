import assert from 'node:assert/strict'
import { test } from 'node:test'
import type { PendingInvite, Person } from './people.ts'
import { parseInvites } from './people.ts'
import {
  closingRooms, earlier, followAgents, hiddenFromNames, hiddenMarkerText, inTheRoom, invitedRows, inviteJoinsLine, isFirstJoin,
  joinLines, needsKai, pickerGroups, roomBlock, roomInviteStart, roomParts, waitingAgent, wontAnswer, askedLine, type RoomAgent,
} from './room-people.ts'

const planner: RoomAgent = { id: 'planner', name: 'planner' }
const coder: RoomAgent = { id: 'coder', name: 'coder' }
const agents = [planner, coder]

const person = (personId: string, name: string, held: string[], extra: Partial<Person> = {}): Person => ({
  personId, name, agents: Object.fromEntries(held.map((a) => [a, 'guest' as const])), everyoneAgents: {}, createdAt: 1, projects: [],
  joined: true, openInvites: 0, devices: [], autoBots: false, revoked: false, ...extra,
})
const ben = person('ben', 'Ben', ['planner', 'coder'])
const ann = person('ann', 'Ann', ['planner'])
const mia = person('mia', 'Mia', [], { joined: false })

test('the picker groups people by whether they hold every room agent, and hides revoked and present people', () => {
  const stays = [{ person: 'ben', joinedGen: 5, leftGen: null }]
  const revoked = person('rev', 'Rev', ['planner', 'coder'], { revoked: true })
  const g = pickerGroups([ben, ann, mia, revoked, person('zed', 'Zed', ['planner', 'coder'])], stays, agents)
  assert.deepEqual(g.canJoin.map((r) => r.person.personId), ['zed'])
  assert.deepEqual(g.needsAccess.map((r) => r.person.personId), ['ann', 'mia'])
  assert.deepEqual(g.needsAccess[0]!.missing.map((a) => a.id), ['coder'])
  assert.equal(g.needsAccess[1]!.accepted, false)
})

test('everyone-agents count as held', () => {
  const p = person('p', 'P', ['planner'], { everyoneAgents: { coder: 'guest' } })
  assert.equal(pickerGroups([p], [], agents).canJoin.length, 1)
})

test('needs-Kai rows: a lost grant, a revoked person, several lost', () => {
  const stays = [{ person: 'ann', joinedGen: 3 }, { person: 'ben', joinedGen: 4 }, { person: 'old', joinedGen: 1, leftGen: 2 }]
  const rows = needsKai(stays, [ann, ben, person('old', 'Old', [])], agents)
  assert.deepEqual(rows.map((r) => [r.personId, r.kind]), [['ann', 'missing']])
  const revoked = needsKai(stays, [ann, { ...ben, revoked: true }], agents)
  assert.deepEqual(revoked.map((r) => r.kind), ['missing', 'revoked'])
  assert.deepEqual(inTheRoom(stays, [ann, ben], agents).map((p) => p.personId), ['ben'])
})

test("the won't-answer line: one agent, two people, revoked", () => {
  const one = wontAnswer(needsKai([{ person: 'ann', joinedGen: 1 }], [ann], agents), agents)!
  assert.equal(one.line, "coder won't answer here. Ann doesn't have coder.")
  const noPlanner = person('cy', 'Cy', [])
  const two = wontAnswer(needsKai([{ person: 'ann', joinedGen: 1 }, { person: 'cy', joinedGen: 1 }], [ann, noPlanner], agents), agents)!
  assert.equal(two.line, "planner and coder won't answer here. Ann and Cy are missing access.")
  const rev = wontAnswer(needsKai([{ person: 'ben', joinedGen: 1 }], [{ ...ben, revoked: true }], agents), agents)!
  assert.equal(rev.line, "No agent answers here while Ben's access is revoked.")
  assert.equal(wontAnswer([], agents), null)
})

test('the hidden marker shows only below the person\'s join line', () => {
  const stays = [{ person: 'ben', joinedGen: 10, leftGen: null }, { person: 'ann', joinedGen: 20, leftGen: null }]
  const name = (id: string) => (id === 'ben' ? 'Ben' : 'Ann')
  const msg = (seq: number, hiddenFrom: string[]) => ({ seq, kind: 'agent', data: { hiddenFrom } })
  assert.deepEqual(hiddenFromNames(msg(9, ['ben']), stays, name), [])
  assert.deepEqual(hiddenFromNames(msg(11, ['ben']), stays, name), ['Ben'])
  assert.deepEqual(hiddenFromNames(msg(25, ['ben', 'ann']), stays, name), ['Ben', 'Ann'])
  assert.deepEqual(hiddenFromNames({ seq: 25, kind: 'user', data: {} }, stays, name), [])
  assert.equal(hiddenMarkerText(['Ben']), 'From before Ben joined')
  assert.equal(hiddenMarkerText(['Ben', 'Ann']), 'From before Ben and Ann joined')
  assert.equal(hiddenMarkerText(['Ben', 'Ann', 'Cy']), 'Hidden from 3 people')
})

test('join lines: the first join adds the @ rule, a leave is one line', () => {
  const joined = (seq: number) => ({ seq, kind: 'notice', data: { notice: 'personJoined', person: { id: 'ben', name: 'Ben' } } })
  const a = joined(4)
  const b = joined(9)
  assert.equal(isFirstJoin([a, b], a), true)
  assert.equal(isFirstJoin([a, b], b), false)
  assert.deepEqual(joinLines(a, true), ['Ben joined · sees from here', "Agents now answer only when @'d."])
  assert.deepEqual(joinLines(b, false), ['Ben joined · sees from here'])
  assert.deepEqual(joinLines({ seq: 12, kind: 'notice', data: { notice: 'personLeft', person: { id: 'ben', name: 'Ben' } } }, false), ['Ben left'])
})

test('the invite sheet: room switches follow the agent switches', () => {
  const studio = { id: 'room-1', name: 'Studio', agents }
  const start = roomInviteStart(studio)
  assert.deepEqual(start, { roles: { planner: 'guest', coder: 'guest' }, rooms: ['room-1'] })
  assert.deepEqual(followAgents(['room-1'], [studio], { planner: 'guest' }), [])
  assert.deepEqual(followAgents(['room-1'], [studio], start.roles), ['room-1'])
  assert.equal(inviteJoinsLine('Mia', ['Studio']), 'Mia joins Studio when Mia accepts.')
  assert.equal(inviteJoinsLine('Mia', ['Studio', 'Launch']), 'Mia joins Studio and Launch when Mia accepts.')
  assert.equal(inviteJoinsLine('Mia', []), null)
})

test('settings warn about rooms that would close', () => {
  const rooms = [{ room: 'r', name: 'Studio', agents }]
  assert.deepEqual(closingRooms(ben, { planner: 'guest' }, rooms).map((c) => c.lost.map((a) => a.id)), [['coder']])
  assert.deepEqual(closingRooms(ben, { planner: 'guest', coder: 'member' }, rooms), [])
})

test('a room with an outside bot, or no agents, takes no people', () => {
  const facts = new Map([['a', { id: 'a', name: 'planner', hqAgent: 'planner' }]])
  const parts = roomParts(['a', 'grok'], (id) => facts.get(id), () => 'Grok')
  assert.deepEqual(roomBlock(parts), { kind: 'outsideBot', name: 'Grok' })
  assert.equal(roomBlock(roomParts(['a'], (id) => facts.get(id), () => 'x')), null)
  assert.deepEqual(roomBlock(roomParts([], () => undefined, () => 'x')), { kind: 'noAgents' })
})

test('invited rows: waiting, accepted, ran out, and done parts are left out', () => {
  const inv = (over: Partial<PendingInvite>): PendingInvite => ({ inviteId: 'i', personId: 'mia', agents: {}, createdAt: 1, expiresAt: 1000, usedAt: null, revokedAt: null, rooms: [{ room: 'r', done: false }], ...over })
  const row = (i: PendingInvite, now = 10) => invitedRows([i], [mia], 'r', agents, now)[0]
  assert.equal(row(inv({}))?.state, 'waiting')
  assert.equal(row(inv({ usedAt: 5 }))?.state, 'accepted')
  assert.deepEqual(row(inv({ usedAt: 5 }))?.missing.map((a) => a.id), ['planner', 'coder'])
  assert.equal(row(inv({}), 2000)?.state, 'ranOut')
  assert.equal(row(inv({ revokedAt: 3 }))?.state, 'ranOut')
  assert.equal(row(inv({ rooms: [{ room: 'r', done: true }] })), undefined)
  assert.equal(row(inv({ rooms: [{ room: 'other', done: false }] })), undefined)
  assert.deepEqual(earlier([{ person: 'ben', joinedGen: 1, leftGen: 2 }, { person: 'ann', joinedGen: 1 }], [ben, ann]).map((p) => p.personId), ['ben'])
})

test('parseInvites reads each invite\'s rooms', () => {
  const [i] = parseInvites({ invites: [{ inviteId: 'a', personId: 'p', rooms: [{ room: 'r', done: true }] }] })
  assert.deepEqual(i!.rooms, [{ room: 'r', done: true }])
  assert.equal(parseInvites({ invites: [{ inviteId: 'b' }] })[0]!.rooms, undefined)
})

test('the waiting notice is found by its machine kind or its words', () => {
  const n = (data: object) => ({ seq: 1, kind: 'notice', data })
  assert.equal(waitingAgent(n({ text: 'planner is waiting for the owner to answer a card.' })), 'planner')
  assert.equal(waitingAgent(n({ notice: 'roomCardWaiting', agent: 'coder' })), 'coder')
  assert.equal(waitingAgent(n({ text: 'something else' })), null)
})

test('"Asked planner" until an agent reply lands', () => {
  const u = (text: string) => ({ kind: 'user', seq: 1, data: { text } })
  assert.equal(askedLine([u('hi @planner check')], ['planner', 'coder']), 'Asked planner')
  assert.equal(askedLine([u('hi @planner'), { kind: 'agent', seq: 2, data: {} }], ['planner']), null)
  assert.equal(askedLine([u('no mention')], ['planner']), null)
})
