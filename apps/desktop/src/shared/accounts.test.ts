import assert from 'node:assert/strict'
import { test } from 'node:test'
import {
  addableTo, agentNames, isExtra, move, parseAccountAdd, parseAccounts, setIds, statusText, windowText, withId, withoutId,
} from './accounts.ts'

const sample = {
  policy: 'spread',
  crossHarness: true,
  accounts: [
    { id: 'codex', harness: 'codex', account: null, kind: 'subscription', inSet: true, order: 1, status: 'ready', ready: true, usage: { readAt: 5, windows: [{ name: '5-hour', percent: 41.6, resetsAt: 9_000_000 }, { bad: 1 }] }, pins: [{ agent: 'house', only: true }] },
    { id: 'claude:work', harness: 'claude', account: 'work', kind: 'subscription', inSet: true, order: 0, status: 'out', ready: false, outUntil: 2_000_000, reason: 'rate_limit', pins: [] },
    { id: 'claude:spare', harness: 'claude', account: 'spare', inSet: false, order: null, status: 'signed_out', why: 'not logged in', fix: 'claude auth login', loggedIn: false },
    { bad: true },
  ],
  agents: {
    house: { harness: 'claude', pin: { id: 'codex', only: true }, own: null, order: ['codex', 'claude:work'], main: 'codex', candidates: ['codex'], allOut: false, crossHarness: true, skipped: [{ id: 'claude:work', status: 'out', outUntil: 2_000_000, reason: 'rate_limit' }] },
    lone: { own: ['claude:work'], order: ['claude:work'], allOut: true },
  },
}

test('the read parses and leaves out what it cannot read', () => {
  const a = parseAccounts(sample)
  assert.equal(a.policy, 'spread')
  assert.equal(a.crossHarness, true)
  assert.equal(a.accounts.length, 3)
  assert.deepEqual(a.accounts[0]!.usage!.windows, [{ name: '5-hour', percent: 41.6, resetsAt: 9_000_000 }])
  assert.deepEqual(a.accounts[0]!.pins, [{ agent: 'house', only: true }])
  assert.equal(a.accounts[1]!.outUntil, 2_000_000)
  assert.equal(a.accounts[2]!.loggedIn, false)
  assert.equal(a.agents.house!.skipped[0]!.reason, 'rate_limit')
  assert.equal(a.agents.house!.own, null)
  assert.deepEqual(a.agents.lone!.own, ['claude:work'])
  assert.equal(a.agents.lone!.allOut, true)
})

test('garbage reads as an empty set', () => {
  for (const raw of [null, 'x', [], {}, { accounts: 5, agents: 'no' }]) {
    const a = parseAccounts(raw)
    assert.deepEqual([a.policy, a.crossHarness, a.accounts, a.agents], ['fill', false, [], {}])
  }
})

test('the set is read in its order, and edits keep the rest', () => {
  const a = parseAccounts(sample)
  assert.deepEqual(setIds(a), ['claude:work', 'codex'])
  assert.deepEqual(move(['a', 'b', 'c'], 0, 2), ['b', 'c', 'a'])
  assert.deepEqual(move(['a', 'b'], 0, 5), ['a', 'b'])
  assert.deepEqual(withId(['a'], 'b'), ['a', 'b'])
  assert.deepEqual(withId(['a'], 'a'), ['a'])
  assert.deepEqual(withoutId(['a', 'b'], 'a'), ['b'])
  assert.deepEqual(addableTo(a, ['codex']).map((x) => x.id), ['claude:work', 'claude:spare'])
})

test('an agent only a pin names is listed without a view', () => {
  const a = parseAccounts({ ...sample, accounts: [{ id: 'codex', harness: 'codex', pins: [{ agent: 'ghost', only: false }] }] })
  assert.deepEqual(agentNames(a).map((x) => [x.name, x.view !== null]), [['ghost', false], ['house', true], ['lone', true]])
})

test('status lines say what the user needs', () => {
  const now = 1_000_000
  assert.equal(statusText({ status: 'ready', why: null, outUntil: null, reason: null }, now).tone, 'ok')
  const out = statusText({ status: 'out', why: null, outUntil: 2_000_000, reason: 'rate_limit' }, now)
  assert.match(out.text, /^out until .+\. The provider refused a turn\.$/)
  assert.equal(out.tone, 'out')
  assert.equal(statusText({ status: 'signed_out', why: 'not logged in', outUntil: null, reason: null }).text, 'not logged in')
  assert.equal(statusText({ status: 'invalid', why: null, outUntil: null, reason: null }).tone, 'blocked')
  assert.match(windowText({ name: '5-hour', percent: 41.6, resetsAt: null }), /^5-hour: 42%$/)
})

test('accountAdd answers a warning to confirm, or the login terminal', () => {
  assert.deepEqual(parseAccountAdd({ ok: false, error: 'x', warning: 'Be careful.' }), { kind: 'warning', warning: 'Be careful.' })
  assert.deepEqual(parseAccountAdd({ account: { ok: true, id: 'codex:deep' }, term: 't1' }), { kind: 'term', term: 't1', id: 'codex:deep' })
  assert.throws(() => parseAccountAdd({ ok: false, error: 'nope' }), /nope/)
  assert.throws(() => parseAccountAdd({}), /terminal/)
})

test('only named logins can be removed', () => {
  const a = parseAccounts(sample)
  assert.deepEqual(a.accounts.map(isExtra), [false, true, true])
})
