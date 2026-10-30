import assert from 'node:assert/strict'
import { test } from 'node:test'
import type { Entry } from '../../../shared/models.ts'
import { buildChat } from './flat-chat.ts'

const entry = (id: string, kind: Entry['kind'], at: number, data: Entry['data'] = {}, threadId: string | null = null, rev = 1): Entry =>
  ({ id, seq: at, botId: 'b', threadId, rev, kind, turn: 1, data, createdAt: at * 1000, updatedAt: 0 })

const ids = (entries: Entry[]) => buildChat(entries).filter((i) => i.kind !== 'separator').map((i) => i.id)

test('task and thread replies interleave with the chat in time order', () => {
  const entries = [
    entry('task', 'user', 1, { text: 'fix it', task: true }),
    entry('chat', 'user', 3, { text: 'meanwhile' }),
    entry('r1', 'agent', 2, { text: 'on it', final: true }, 'task'),
    entry('r2', 'agent', 4, { text: 'done', final: true }, 'task'),
    entry('old', 'agent', 5, { text: 'in an old thread', final: true }, 'chat'),
  ]
  assert.deepEqual(ids(entries), ['task', 'r1', 'chat', 'r2', 'old'])
})

test('an entry in the chat and in a thread fetch shows once, the newer revision', () => {
  const a = entry('a', 'agent', 1, { text: 'old', final: true }, 'task', 1)
  const b = { ...a, rev: 2, data: { text: 'new', final: true } }
  const items = buildChat([a, b, entry('u1', 'user', 2, { clientNonce: 'n' }), entry('u1', 'user', 2, { clientNonce: 'n' })])
  const shown = items.filter((i) => i.kind === 'entry')
  assert.deepEqual(shown.map((i) => i.id), ['a', 'user-n'])
  assert.equal(shown[0]!.kind === 'entry' && shown[0]!.entry.data.text, 'new')
})

test('a run of tool calls is one steps item, with a stable id and every non-delivery entry', () => {
  const tools = Array.from({ length: 7 }, (_, i) => entry(`t${i}`, 'tool', 2 + i, {}, 'task'))
  const items = buildChat([entry('u', 'user', 1, { text: 'go' }), ...tools, entry('a', 'agent', 20, { final: true, text: 'done' })])
  assert.deepEqual(items.filter((i) => i.kind !== 'separator').map((i) => i.kind), ['entry', 'steps', 'entry'])
  const steps = items.find((i) => i.kind === 'steps')!
  assert.equal(steps.id, 'steps-t0')
  assert.equal(steps.kind === 'steps' && steps.entries.length, 7)
  // The run grows: same id, so an open row stays open.
  const more = buildChat([entry('u', 'user', 1, { text: 'go' }), ...tools, entry('t7', 'tool', 9, {}, 'task')])
  assert.equal(more.find((i) => i.kind === 'steps')!.id, 'steps-t0')
})

test('file deliveries are dropped from a run; a run of only those shows nothing', () => {
  const send = (id: string, at: number) => entry(id, 'tool', at, { title: 'mcp__kith-files__send_file', status: 'completed' })
  const mixed = buildChat([entry('u', 'user', 1), entry('t0', 'tool', 2, { title: 'ls' }), send('s', 3), entry('a', 'agent', 4, { final: true })])
  const steps = mixed.find((i) => i.kind === 'steps')!
  assert.deepEqual(steps.kind === 'steps' && steps.entries.map((e) => e.id), ['t0'])
  const only = buildChat([entry('u', 'user', 1), send('s1', 2), send('s2', 3), entry('a', 'agent', 4, { final: true })])
  assert.ok(!only.some((i) => i.kind === 'steps'))
  // Other kith-files tools are not deliveries.
  const call = buildChat([entry('u', 'user', 1), entry('c', 'tool', 2, { title: 'mcp__kith-files__call_person' })])
  assert.ok(call.some((i) => i.kind === 'steps'))
})

test('thinking and unfinished replies stay out; an agent reply after tool steps opens a new group', () => {
  const items = buildChat([
    entry('a1', 'agent', 1, { final: true, author: 'x' }),
    entry('th', 'thought', 2),
    entry('p', 'agent', 3, { text: 'partial' }),
    entry('t', 'tool', 4),
    entry('a2', 'agent', 5, { final: true, author: 'x' }),
  ])
  assert.deepEqual(items.filter((i) => i.kind !== 'separator').map((i) => i.id), ['a1', 'steps-t', 'a2'])
  assert.deepEqual(items.filter((i) => i.kind === 'entry').map((i) => i.kind === 'entry' && i.groupStart), [true, true])
})

test('author grouping and time separators', () => {
  const items = buildChat([entry('u1', 'user', 1), entry('u2', 'user', 2), entry('a', 'agent', 3, { final: true, author: 'x' }), entry('late', 'user', 10_000)])
  assert.deepEqual(items.map((i) => i.kind === 'separator' ? 'sep' : i.id), ['sep', 'u1', 'u2', 'a', 'sep', 'late'])
  assert.deepEqual(items.filter((i) => i.kind === 'entry').map((i) => i.kind === 'entry' && i.groupStart), [true, false, true, true])
})

test('tools before a gap of over an hour stay above the next message\'s separator', () => {
  const items = buildChat([entry('a', 'agent', 1, { final: true, author: 'x' }), entry('t', 'tool', 2), entry('b', 'agent', 20_000, { final: true, author: 'x' })])
  assert.deepEqual(items.map((i) => (i.kind === 'separator' ? 'sep' : i.id)), ['sep', 'a', 'steps-t', 'sep', 'b'])
})
