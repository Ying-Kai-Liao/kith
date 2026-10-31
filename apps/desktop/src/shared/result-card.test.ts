import assert from 'node:assert/strict'
import { test } from 'node:test'
import { KNOWN_CARDS, richCardOf } from './rich-card.ts'
import type { RichCard } from './rich-card.ts'
import { resultView } from './result-card.ts'

const card = (snapshot: Record<string, unknown>): RichCard => ({ type: 'result', v: 1, snapshot })
const v2 = (snapshot: Record<string, unknown>): RichCard => ({ type: 'result', v: 2, snapshot })

test('a Result card has its title, facts and the reply\'s links', () => {
  const v = resultView(card({
    title: 'Disk check on the Mac mini',
    facts: ['Free space: 340 GB of 500 GB', 'Memory: 12 GB in use'],
    links: [{ label: 'Open report', url: 'https://example.com/report/1' }],
  }))
  assert.deepEqual(v, {
    title: 'Disk check on the Mac mini',
    sub: '',
    rows: [],
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

test('a v2 Result card has its title, sub line, Key: value rows and the reply\'s link', () => {
  const v = resultView(v2({
    title: 'QF 432 · Melbourne to Sydney',
    sub: 'On time so far.',
    rows: [{ label: 'Departs', value: 'Fri 31 Oct · 5:35 pm' }, { label: 'Lands', value: '7:05 pm' }],
    links: [{ label: 'qantas.com/flight-status', url: 'https://qantas.com/flight-status' }],
  }))
  assert.deepEqual(v, {
    title: 'QF 432 · Melbourne to Sydney',
    sub: 'On time so far.',
    rows: [{ label: 'Departs', value: 'Fri 31 Oct · 5:35 pm' }, { label: 'Lands', value: '7:05 pm' }],
    facts: [],
    links: [{ label: 'qantas.com/flight-status', url: 'https://qantas.com/flight-status' }],
  })
})

test('a v2 card needs a title and a row; the sub line is optional', () => {
  assert.equal(resultView(v2({ rows: [{ label: 'a', value: 'b' }] })), null)
  assert.equal(resultView(v2({ title: 'T' })), null)
  assert.equal(resultView(v2({ title: 'T', rows: [{ label: 'a', value: ' ' }, 'nope', 3] })), null)
  assert.equal(resultView(v2({ title: 'T', facts: ['a'] })), null)
  assert.equal(resultView({ type: 'result', v: 2 }), null)
  assert.equal(resultView(v2({ title: 'T', rows: [{ label: 'a', value: 'b' }] }))?.sub, '')
})

test('a v2 row with no label draws its value alone, and only the first five rows show', () => {
  const rows = ['1', '2', '3', '4', '5', '6'].map((value) => ({ label: '', value }))
  const v = resultView(v2({ title: 'T', rows }))
  assert.equal(v?.rows.length, 5)
  assert.deepEqual(v?.rows[0], { label: '', value: '1' })
})

test('v2 links are web links only, at most two, and an unlabelled one shows its address', () => {
  const v = resultView(v2({
    title: 'T',
    rows: [{ label: 'a', value: 'b' }],
    links: [{ label: 'x', url: 'javascript:alert(1)' }, { url: 'https://a.example/1' }, { label: 'B', url: 'http://b.example' }, { label: 'C', url: 'https://c.example' }],
  }))
  assert.deepEqual(v?.links, [{ label: 'a.example/1', url: 'https://a.example/1' }, { label: 'B', url: 'http://b.example' }])
})

test('this app draws the Result card of versions 1 and 2; a newer one is the plain reply', () => {
  assert.equal(KNOWN_CARDS.result, 2)
  assert.ok(richCardOf({ text: 'x', richCard: { type: 'result', v: 1 } }))
  assert.ok(richCardOf({ text: 'x', richCard: { type: 'result', v: 2 } }))
  assert.equal(richCardOf({ text: 'x', richCard: { type: 'result', v: 3 } }), null)
})
