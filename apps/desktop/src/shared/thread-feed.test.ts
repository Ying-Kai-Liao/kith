import assert from 'node:assert/strict'
import { test } from 'node:test'
import type { Entry } from './models.ts'
import { toolRowOf } from './thread-feed.ts'

const entry = (id: string, kind: Entry['kind'], data: Entry['data'] = {}): Entry =>
  ({ id, seq: 0, botId: 'b', threadId: 'r', rev: 1, kind, turn: 1, data, createdAt: 0, updatedAt: 0 })

test('tool rows show the verb, file and diff size, or the failure', () => {
  const diff = { path: '/a/b/session.py', added: 6, removed: 2, isNew: false, startLine: 1, patch: '@@' }
  const edit = toolRowOf(entry('e', 'tool', { toolKind: 'edit', status: 'completed', diffs: [diff, diff] }))
  assert.deepEqual([edit.verb, edit.object, edit.result, edit.state], ['Edited', 'session.py +1', '+12 -4', 'done'])
  const run = toolRowOf(entry('r', 'tool', { toolKind: 'execute', status: 'failed', title: 'npm test\nmore', output: 'x' }))
  assert.deepEqual([run.verb, run.object, run.result, run.state, run.command], ['Ran', 'npm test', 'failed', 'error', 'npm test\nmore'])
  assert.equal(toolRowOf(entry('w', 'tool', { status: 'in_progress' })).result, undefined)
})
