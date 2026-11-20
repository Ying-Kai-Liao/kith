import assert from 'node:assert/strict'
import { test } from 'node:test'
import { partialUpdate, sendOrRollback } from './optimistic.ts'

test('a one-setting change sends only the id and that setting', () => {
  assert.deepEqual(partialUpdate('b1', { pinned: true }), { id: 'b1', pinned: true })
  assert.deepEqual(Object.keys(partialUpdate('b1', { hidden: false })).sort(), ['hidden', 'id'])
})

test('a failed send rolls back and reports; a good one does neither', async () => {
  const seen: string[] = []
  let rolled = 0
  await sendOrRollback(async () => { throw new Error('unknown bot') }, () => { rolled++ }, (m) => seen.push(m))
  assert.equal(rolled, 1)
  assert.deepEqual(seen, ['unknown bot'])
  await sendOrRollback(async () => {}, () => { rolled++ }, (m) => seen.push(m))
  assert.equal(rolled, 1)
  assert.equal(seen.length, 1)
})
