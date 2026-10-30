import assert from 'node:assert/strict'
import { test } from 'node:test'
import type { Entry } from './models.ts'
import { foldTopics, inTopic, plainFirstLine, type Topic } from './topics.ts'

const topic = (over: Partial<Topic> = {}): Topic => ({ id: 't', title: 'Pace', untilId: 'c', untilAt: 30, turns: [], createdAt: 10, ...over })
const entry = (id: string, at: number, turn = 1, data: Entry['data'] = {}): Entry =>
  ({ id, seq: at, botId: 'b', threadId: null, rev: 1, kind: 'agent', turn, data, createdAt: at, updatedAt: at })

test('an entry between the root and untilAt is in; same millisecond counts at both ends', () => {
  const root = { createdAt: 10, turn: 1 }
  assert.equal(inTopic(root, topic(), { createdAt: 20, turn: 2 }), true)
  assert.equal(inTopic(root, topic(), { createdAt: 10, turn: 2 }), true)
  assert.equal(inTopic(root, topic(), { createdAt: 30, turn: 2 }), true)
})

test('before the root and after untilAt are out', () => {
  const root = { createdAt: 10, turn: 1 }
  assert.equal(inTopic(root, topic(), { createdAt: 9, turn: 2 }), false)
  assert.equal(inTopic(root, topic(), { createdAt: 31, turn: 2 }), false)
})

test('a turn the topic holds follows it past untilAt, but never from before the root', () => {
  const root = { createdAt: 10, turn: 1 }
  const t = topic({ turns: [5] })
  assert.equal(inTopic(root, t, { createdAt: 99, turn: 5 }), true)
  assert.equal(inTopic(root, t, { createdAt: 5, turn: 5 }), false)
  assert.equal(inTopic(root, t, { createdAt: 99, turn: 6 }), false)
})

test('foldTopics folds the run under its root and leaves the rest', () => {
  const flat = [entry('a', 5), entry('root', 10, 1, { topic: topic() }), entry('b', 20), entry('c', 30), entry('d', 40)]
  const { folds, hidden } = foldTopics(flat)
  assert.deepEqual(folds.get('root')!.map((e) => e.id), ['root', 'b', 'c'])
  assert.deepEqual([...hidden].map((e) => e.id), ['b', 'c'])
})

test('a root that is not loaded folds nothing', () => {
  const { folds, hidden } = foldTopics([entry('b', 20), entry('c', 30)])
  assert.equal(folds.size, 0)
  assert.equal(hidden.size, 0)
})

test('a topic root inside another topic is an ordinary message', () => {
  const flat = [entry('r1', 10, 1, { topic: topic() }), entry('r2', 20, 1, { topic: topic({ id: 'u', createdAt: 20 }) }), entry('x', 25)]
  const { folds } = foldTopics(flat)
  assert.deepEqual([...folds.keys()], ['r1'])
  assert.deepEqual(folds.get('r1')!.map((e) => e.id), ['r1', 'r2', 'x'])
})

test('plainFirstLine strips inline markdown and keeps the words', () => {
  assert.equal(plainFirstLine('Run `npm test` **now** and *then* _again_'), 'Run npm test now and then again')
  assert.equal(plainFirstLine('See [the docs](https://x.io/a_b) ok'), 'See the docs ok')
  assert.equal(plainFirstLine('## Heading here'), 'Heading here')
  assert.equal(plainFirstLine('> quoted'), 'quoted')
  assert.equal(plainFirstLine('- item'), 'item')
  assert.equal(plainFirstLine('1. first'), 'first')
  assert.equal(plainFirstLine('`only code`'), 'only code')
  assert.equal(plainFirstLine('snake_case_name stays'), 'snake_case_name stays')
})

test('plainFirstLine takes the first line only and is empty for bare markers', () => {
  assert.equal(plainFirstLine('one **two**\nthree'), 'one two')
  assert.equal(plainFirstLine('##'), '')
  assert.equal(plainFirstLine('---'), '')
  assert.equal(plainFirstLine('  \n x'), 'x')
  assert.equal(plainFirstLine(undefined), '')
})
