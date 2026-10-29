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

test('a title that already opens with the verb does not double it', () => {
  const obj = (toolKind: string, title: string) => toolRowOf(entry('t', 'tool', { toolKind, title })).object
  assert.equal(obj('read', 'Read src/pace.js'), 'src/pace.js')
  assert.equal(obj('read', 'reading src/pace.js'), 'src/pace.js')
  assert.equal(obj('edit', 'Write src/pace.js'), 'src/pace.js')
  assert.equal(obj('search', 'Searched for foo'), 'for foo')
  assert.equal(obj('fetch', 'Fetch https://x.dev'), 'https://x.dev')
  assert.equal(obj('read', 'src/pace.js'), 'src/pace.js')
  assert.equal(obj('read', 'Read'), 'Read')
  assert.equal(obj('read', 'Ran tests'), 'Ran tests')
  assert.equal(toolRowOf(entry('x', 'tool', { toolKind: 'execute', title: 'Run npm test' })).object, 'Run npm test')
})
