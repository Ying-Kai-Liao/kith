import assert from 'node:assert/strict'
import { test } from 'node:test'
import { groupReactions } from './reaction-groups.ts'

test('reactions: identical emoji group with a count, first-seen order', () => {
  assert.deepEqual(groupReactions(['👍', '❤️', '👍']), [
    { emoji: '👍', count: 2, mine: true },
    { emoji: '❤️', count: 1, mine: true },
  ])
})

test('reactions: none gives no chips', () => {
  assert.deepEqual(groupReactions(undefined), [])
  assert.deepEqual(groupReactions([]), [])
})
