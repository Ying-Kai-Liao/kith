import assert from 'node:assert/strict'
import { test } from 'node:test'
import { levelLabel, mockSharedWithYou, sharedAgentRows } from './shared-with-you.ts'

test('role maps to the two levels, unknown to none', () => {
  assert.equal(levelLabel('guest'), 'Can chat')
  assert.equal(levelLabel('member'), 'Can work')
  assert.equal(levelLabel(undefined), null)
})

test('rows put Can work first, then by name; no role gives names only', () => {
  const rows = sharedAgentRows([
    { agent: 'b', name: 'Beta', role: 'guest' },
    { agent: 'z', name: null, role: 'member' },
    { agent: 'a', name: 'Alpha', role: 'guest' },
  ])
  assert.deepEqual(rows.map((r) => [r.name, r.level]), [['z', 'Can work'], ['Alpha', 'Can chat'], ['Beta', 'Can chat']])
  assert.equal(sharedAgentRows([{ agent: 'x', name: 'X' }])[0].level, null)
  assert.deepEqual(sharedAgentRows([]), [])
})

test('the mock provider shows no rooms', () => assert.deepEqual(mockSharedWithYou.rooms('c'), []))
