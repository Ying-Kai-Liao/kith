import assert from 'node:assert/strict'
import { test } from 'node:test'
import { noticeThreadChip } from './notice-thread.ts'

test('only a notice with replies gets a chip', () => {
  const thread = { count: 2, unread: 1, authors: [], lastAt: 0 }
  assert.equal(noticeThreadChip({ kind: 'notice', data: { thread } } as never), true)
  assert.equal(noticeThreadChip({ kind: 'notice', data: { thread: { ...thread, count: 0 } } } as never), false)
  assert.equal(noticeThreadChip({ kind: 'notice', data: { text: 'plain' } } as never), false)
  assert.equal(noticeThreadChip({ kind: 'user', data: { thread } } as never), false)
})
