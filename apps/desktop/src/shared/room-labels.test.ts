import assert from 'node:assert/strict'
import { test } from 'node:test'
import type { Bot } from './models.ts'
import { MAX_LABELS, hasLabel, labelsInUse, parseLabels } from './room-labels.ts'

const bot = (kind: string, labels?: string[]) => ({ kind, labels }) as Bot

test('typed labels are lowercased, trimmed, deduplicated and capped like the host does', () => {
  assert.deepEqual(parseLabels(' Launch, q4  Plan ,,launch\nOps'), ['launch', 'q4 plan', 'ops'])
  assert.equal(parseLabels('a,b,c,d,e,f,g').length, MAX_LABELS)
  assert.equal(parseLabels('x'.repeat(40))[0]!.length, 24)
  assert.deepEqual(parseLabels(''), [])
})

test('the filter row lists the labels rooms use, most used first', () => {
  const bots = [bot('group', ['ops', 'launch']), bot('group', ['launch']), bot('group'), bot('agent', ['stray'])]
  assert.deepEqual(labelsInUse(bots), ['launch', 'ops'])
})

test('a label filter keeps the rooms that carry it and no agent', () => {
  assert.equal(hasLabel(bot('group', ['ops']), 'ops'), true)
  assert.equal(hasLabel(bot('group', ['ops']), 'launch'), false)
  assert.equal(hasLabel(bot('group'), 'ops'), false)
  assert.equal(hasLabel(bot('agent'), 'ops'), false)
  assert.equal(hasLabel(bot('agent'), null), true)
})
