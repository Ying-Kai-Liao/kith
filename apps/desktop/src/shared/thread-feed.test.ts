import assert from 'node:assert/strict'
import { test } from 'node:test'
import type { Entry } from './models.ts'
import { threadFeed, toolRowOf, VISIBLE_STEPS } from './thread-feed.ts'

const entry = (id: string, kind: Entry['kind'], data: Entry['data'] = {}): Entry =>
  ({ id, seq: 0, botId: 'b', threadId: 'r', rev: 1, kind, turn: 1, data, createdAt: 0, updatedAt: 0 })

test('a long run of tool calls folds into its newest steps and a count', () => {
  const tools = Array.from({ length: 7 }, (_, i) => entry(`t${i}`, 'tool'))
  const feed = threadFeed([entry('u', 'user', { text: 'go' }), ...tools, entry('a', 'agent', { final: true, text: 'done' })])
  assert.deepEqual(feed.map((f) => f.kind), ['entry', 'earlier', 'tool', 'tool', 'tool', 'tool', 'entry'])
  const earlier = feed[1]!
  assert.equal(earlier.kind === 'earlier' && earlier.count, 7 - VISIBLE_STEPS)
})

test('a short run stays whole; thinking and unfinished replies stay out', () => {
  const feed = threadFeed([entry('t0', 'tool'), entry('th', 'thought'), entry('p', 'agent', { text: 'partial' }), entry('t1', 'tool')])
  assert.deepEqual(feed.map((f) => f.id), ['t0', 't1'])
})

test('author grouping and a stable id for the sender\'s own message', () => {
  const feed = threadFeed([entry('u1', 'user', { clientNonce: 'n' }), entry('u2', 'user'), entry('a', 'agent', { final: true, author: 'x' })])
  assert.deepEqual(feed.map((f) => [f.id, f.kind === 'entry' && f.groupStart]), [['user-n', true], ['u2', false], ['a', true]])
})

test('an agent reply after tool steps starts a new group', () => {
  const feed = threadFeed([entry('a1', 'agent', { final: true, author: 'x' }), entry('t', 'tool'), entry('a2', 'agent', { final: true, author: 'x' })])
  assert.deepEqual(feed.filter((f) => f.kind === 'entry').map((f) => f.kind === 'entry' && f.groupStart), [true, true])
})

test('tool rows show the verb, file and diff size, or the failure', () => {
  const diff = { path: '/a/b/session.py', added: 6, removed: 2, isNew: false, startLine: 1, patch: '@@' }
  const edit = toolRowOf(entry('e', 'tool', { toolKind: 'edit', status: 'completed', diffs: [diff, diff] }))
  assert.deepEqual([edit.verb, edit.object, edit.result, edit.state], ['Edited', 'session.py +1', '+12 -4', 'done'])
  const run = toolRowOf(entry('r', 'tool', { toolKind: 'execute', status: 'failed', title: 'npm test\nmore', output: 'x' }))
  assert.deepEqual([run.verb, run.object, run.result, run.state, run.command], ['Ran', 'npm test', 'failed', 'error', 'npm test\nmore'])
  assert.equal(toolRowOf(entry('w', 'tool', { status: 'in_progress' })).result, undefined)
})
