import assert from 'node:assert/strict'
import { test } from 'node:test'
import { planView } from './plan-card.ts'
import { richCardOf, KNOWN_CARDS } from './rich-card.ts'

const card = (snapshot: Record<string, unknown>) => ({ type: 'plan', v: 1, ref: { entry: 'e1' }, snapshot })

test('the plan card shows the count, the bar and each step in its state', () => {
  const view = planView(card({ title: 'Plan', done: 2, total: 4, ended: false, steps: [
    { text: 'Read', state: 'done' }, { text: 'Edit', state: 'done' }, { text: 'Test', state: 'now' }, { text: 'Ship', state: 'next' },
  ] }))
  assert.equal(view.count, '2 of 4')
  assert.equal(view.fraction, 0.5)
  assert.deepEqual(view.steps.map((s) => s.state), ['done', 'done', 'now', 'next'])
  assert.equal(view.ended, false)
})

test('a plan that ended unfinished keeps a stopped step and an unknown state reads as next', () => {
  const view = planView(card({ done: 1, total: 3, ended: true, steps: [
    { text: 'a', state: 'done' }, { text: 'b', state: 'stopped' }, { text: 'c', state: 'sideways' },
  ] }))
  assert.deepEqual(view.steps.map((s) => s.state), ['done', 'stopped', 'next'])
  assert.equal(view.ended, true)
})

test('a plan with no steps or odd numbers does not break', () => {
  const empty = planView(card({}))
  assert.equal(empty.count, '0 of 0')
  assert.equal(empty.fraction, 0)
  assert.equal(planView(card({ done: 9, total: 2, steps: [{ text: 'x', state: 'done' }, 5, { text: '' }] })).count, '2 of 2')
})

test('this app draws the plan card, and an app that does not know it shows the plain notice', () => {
  const data = { text: 'Plan · 1 of 2', richCard: card({ done: 1, total: 2, steps: [] }) } as never
  assert.ok(richCardOf(data))
  assert.equal(richCardOf(data, { schedule: 1, news: 1 }), null)
  assert.equal(KNOWN_CARDS.plan, 1)
})
