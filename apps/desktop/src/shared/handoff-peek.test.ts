import assert from 'node:assert/strict'
import { test } from 'node:test'
import { askIdOf, joinBlocked, limitParts, parsePeek, parsePeeks, sendBlocked, stopBlocked } from './handoff-peek.ts'

const raw = (over: object = {}) => ({
  id: 'bg-20261031120000-abc123',
  state: 'working',
  brief: 'Write the plan',
  from: { agent: 'architect', person: 'mia' },
  to: 'echo',
  crossed: false,
  thread: { botId: 'echo', threadId: 't1', sessionId: 's' },
  live: { kind: 'tool', text: 'Edited plan.md' },
  limits: { costMicroUsd: 250_000, capMicroUsd: 5_000_000, chainCostMicroUsd: 700_000, hop: 1, hopMax: 8, crossHops: 0, crossMax: 2, elapsedMs: 90_000 },
  startedMs: 1000,
  answeredMs: null,
  cappedMs: null,
  ...over,
})

test('a peek reads as the host sent it', () => {
  const p = parsePeek(raw())!
  assert.equal(p.state, 'working')
  assert.deepEqual(p.thread, { botId: 'echo', threadId: 't1' })
  assert.equal(p.live?.text, 'Edited plan.md')
})

test('an unknown state reads as unknown, a null thread and live stay null, junk has no peek', () => {
  const p = parsePeek(raw({ state: 'weird', thread: null, live: null }))!
  assert.equal(p.state, 'unknown')
  assert.equal(p.thread, null)
  assert.equal(p.live, null)
  assert.equal(parsePeek({ state: 'working' }), null)
  assert.deepEqual(parsePeeks({ peeks: [raw(), 3] }).map((x) => x.id), ['bg-20261031120000-abc123'])
  assert.deepEqual(parsePeeks(null), [])
})

test('the ask id comes from a kith ask row: --bg output, --wait, a waiting run; plain asks have none', () => {
  const id = 'bg-20261031120000-abc123'
  assert.equal(askIdOf('kith ask --bg echo "x"', `started echo in the background as ${id}; its answer: kith ask --wait ${id}`), id)
  assert.equal(askIdOf(`kith ask --wait ${id}`, ''), id)
  assert.equal(askIdOf('kith ask echo "x"', '{"waiting":true,"id":"ask-20261031120000-0a1b2c"}'), 'ask-20261031120000-0a1b2c')
  assert.equal(askIdOf('kith ask echo "x"', 'the answer'), null)
  assert.equal(askIdOf('ls', id), null)
  assert.equal(askIdOf(undefined, id), null)
})

test('the limits line flags the one closest to its limit', () => {
  const parts = limitParts(parsePeek(raw())!.limits)
  assert.equal(parts[0]!.label, '$0.70 / $5')
  assert.deepEqual(parts.filter((p) => p.watch).map((p) => p.kind), ['budget'])
  const hops = limitParts(parsePeek(raw({ limits: { ...raw().limits, hop: 7 } }))!.limits)
  assert.deepEqual(hops.filter((p) => p.watch).map((p) => p.kind), ['hops'])
  assert.equal(hops.at(-1)!.label, '1 min')
})

test('Join, Stop and the box say why they are off', () => {
  assert.equal(joinBlocked(parsePeek(raw())!), null)
  assert.equal(stopBlocked(parsePeek(raw())!), null)
  const away = parsePeek(raw({ thread: null, crossed: true }))!
  assert.equal(joinBlocked(away), 'It runs on another Mac')
  assert.equal(stopBlocked(away), 'It runs on another Mac')
  const done = parsePeek(raw({ state: 'done' }))!
  assert.equal(joinBlocked(done), null, 'Join still opens a finished thread')
  assert.equal(stopBlocked(done), 'It is not running')
  assert.equal(sendBlocked(done), 'It is not running')
  assert.equal(stopBlocked(parsePeek(raw({ state: 'stopped' }))!), 'It is not running')
})
