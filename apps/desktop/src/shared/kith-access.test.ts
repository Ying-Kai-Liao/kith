import assert from 'node:assert/strict'
import { test } from 'node:test'
import { accessHidden, accountText, agentKeys, groupGrants, keyDetail, keyName, personTitle } from './kith-access.ts'
import type { AccessGrant } from './kith-info.ts'
import type { KeyRow } from './secret-card.ts'

const key = (over: Partial<KeyRow>): KeyRow => ({ profile: 'notion/library', service: 'notion', account: 'library', owner: 'kai', vars: ['NOTION_TOKEN'], agents: ['designer'], lastUsed: null, ...over })
const grant = (over: Partial<AccessGrant>): AccessGrant => ({ key: 'k', kind: 'read', agent: 'designer', person: 'mia', name: 'Mia', target: '~/docs', since: null, ...over })

test('only the keys this agent holds', () => {
  const rows = [key({}), key({ profile: 'github/work', agents: ['scout'] }), key({ profile: 'x', agents: [] })]
  assert.deepEqual(agentKeys(rows, 'designer').map((k) => k.profile), ['notion/library'])
  assert.deepEqual(agentKeys([], 'designer'), [])
})

test('account and key lines', () => {
  assert.equal(accountText(key({})), 'notion · library')
  assert.equal(keyName(key({ vars: ['A', 'B'] })), 'A, B')
  assert.equal(keyName(key({ vars: [] })), 'library')
  const now = Date.parse('2026-10-29T12:00:00Z')
  assert.equal(keyDetail(key({ lastUsed: '2026-10-29T10:00:00Z' }), now), 'Keychain · last used 2 h ago')
  assert.equal(keyDetail(key({ lastUsed: null }), now), 'Keychain · never used')
  assert.equal(keyDetail(key({ lastUsed: 'nonsense' }), now), 'Keychain · never used')
})

test('grants group by person in first-seen order', () => {
  const groups = groupGrants([grant({ key: '1' }), grant({ key: '2', person: '*', name: '' }), grant({ key: '3', kind: 'domain', target: 'a.org' })])
  assert.deepEqual(groups.map((g) => [g.person, g.grants.map((x) => x.key)]), [['mia', ['1', '3']], ['*', ['2']]])
  assert.equal(personTitle(groups[0]!), 'Mia')
  assert.equal(personTitle(groups[1]!), 'Everyone')
  assert.equal(personTitle({ person: 'p1', name: '' }), 'p1')
})

test('a 403 or 404 hides the section', () => {
  assert.equal(accessHidden({ status: 403 }), true)
  assert.equal(accessHidden({ status: 404 }), true)
  assert.equal(accessHidden(new Error('unknown method listKeys')), true)
  assert.equal(accessHidden({ status: 500 }), false)
  assert.equal(accessHidden(null), false)
})
