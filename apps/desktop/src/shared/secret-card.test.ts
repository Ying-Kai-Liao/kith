import assert from 'node:assert/strict'
import { test } from 'node:test'
import type { Entry } from './models.ts'
import { isAccountName, isSecretCard, keysByService, parseKeys, secretOutcome, secretSummary, valueProblem, type SecretRequest } from './secret-card.ts'

const secret: SecretRequest = { service: 'github', vars: ['GITHUB_TOKEN'], profile: null, account: null, purpose: '', replaces: false, state: 'pending' }
const entry = (data: Entry['data'], kind = 'permission') => ({ kind, data })

test('account names follow the host rule', () => {
  for (const ok of ['work', 'a', 'a.b-c_d', '0x']) assert.equal(isAccountName(ok), true, ok)
  for (const bad of ['', 'Work', '-a', '.a', 'a b', 'a/b', 'x'.repeat(41)]) assert.equal(isAccountName(bad), false, bad)
})

test('values: 1..1536 bytes, no NUL, counted in bytes', () => {
  assert.equal(valueProblem('abc'), null)
  assert.equal(valueProblem(''), 'Enter every field of the key')
  assert.equal(valueProblem('a\0b') !== null, true)
  assert.equal(valueProblem('x'.repeat(1536)), null)
  assert.equal(valueProblem('x'.repeat(1537)), 'That key is too long')
  assert.equal(valueProblem('é'.repeat(769)), 'That key is too long')
})

test('a card is a secret card only with data.card and data.secret on a permission entry', () => {
  assert.equal(isSecretCard(entry({ card: 'key', secret })), true)
  assert.equal(isSecretCard(entry({ card: 'account', secret })), true)
  assert.equal(isSecretCard(entry({ secret })), false)
  assert.equal(isSecretCard(entry({ card: 'key' })), false)
  assert.equal(isSecretCard(entry({ title: 'Edit a.js' })), false)
  assert.equal(isSecretCard(entry({ card: 'key', secret }, 'agent')), false)
})

test('the closed states have words and a pending card has none', () => {
  assert.equal(secretOutcome({ ...secret, state: 'saved', profile: 'github/work' }), 'Saved as github/work')
  assert.equal(secretOutcome({ ...secret, state: 'denied' }), 'Not now')
  assert.equal(secretOutcome({ ...secret, state: 'expired' }), 'Expired')
  assert.equal(secretOutcome(secret), null)
})

test('the summary line names the agent and the service, never a field', () => {
  assert.equal(secretSummary('Pacer', 'github'), 'Pacer needs a key for github. Open to add it.')
})

test('listKeys rows are parsed and grouped by service', () => {
  const keys = parseKeys({ keys: [
    { profile: 'github/work', service: 'github', account: 'work', owner: 'kai', vars: ['GITHUB_TOKEN'], agents: ['a'], last_used: '2026-10-25T01:00:00Z' },
    { profile: 'notion/lib', service: 'notion', vars: [], agents: [], last_used: null },
    { profile: 'github/personal', service: 'github', account: 'personal', agents: [] },
    { nope: 1 },
  ] })
  assert.equal(keys.length, 3)
  assert.equal(keys[1]!.account, 'lib')
  assert.deepEqual(keysByService(keys).map((g) => [g.service, g.keys.map((k) => k.account)]), [['github', ['personal', 'work']], ['notion', ['lib']]])
  assert.deepEqual(parseKeys(null), [])
})
