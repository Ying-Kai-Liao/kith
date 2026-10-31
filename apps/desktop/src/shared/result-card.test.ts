import assert from 'node:assert/strict'
import { test } from 'node:test'
import { KNOWN_CARDS, richCardOf } from './rich-card.ts'
import type { RichCard } from './rich-card.ts'
import { resultView } from './result-card.ts'

const card = (snapshot: Record<string, unknown>): RichCard => ({ type: 'result', v: 1, snapshot })

test('a Result card has its title, facts and the reply\'s links', () => {
  const v = resultView(card({
    title: 'Disk check on the Mac mini',
    facts: ['Free space: 340 GB of 500 GB', 'Memory: 12 GB in use'],
    links: [{ label: 'Open report', url: 'https://example.com/report/1' }],
  }))
  assert.deepEqual(v, {
    title: 'Disk check on the Mac mini',
    facts: ['Free space: 340 GB of 500 GB', 'Memory: 12 GB in use'],
    links: [{ label: 'Open report', url: 'https://example.com/report/1' }],
  })
})

test('a card without a title or a fact is the plain reply', () => {
  assert.equal(resultView(card({ facts: ['a'] })), null)
  assert.equal(resultView(card({ title: 'T' })), null)
  assert.equal(resultView(card({ title: 'T', facts: [' ', 3] })), null)
  assert.equal(resultView({ type: 'result', v: 1 }), null)
})

test('only web links become buttons, at most two, unlabelled ones say Open', () => {
  const v = resultView(card({
    title: 'T',
    facts: ['a'],
    links: [{ label: 'x', url: 'javascript:alert(1)' }, { url: 'https://a.example/1' }, 'nope', { label: 'B', url: 'http://b.example' }, { label: 'C', url: 'https://c.example' }],
  }))
  assert.deepEqual(v?.links, [{ label: 'Open', url: 'https://a.example/1' }, { label: 'B', url: 'http://b.example' }])
})

test('facts are capped at four', () => {
  assert.equal(resultView(card({ title: 'T', facts: ['1', '2', '3', '4', '5'] }))?.facts.length, 4)
})

test('this app draws the Result card of version 1; a newer one is the plain reply', () => {
  assert.equal(KNOWN_CARDS.result, 1)
  assert.ok(richCardOf({ text: 'x', richCard: { type: 'result', v: 1 } }))
  assert.equal(richCardOf({ text: 'x', richCard: { type: 'result', v: 2 } }), null)
})
