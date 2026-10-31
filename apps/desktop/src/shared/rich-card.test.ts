import assert from 'node:assert/strict'
import { test } from 'node:test'
import type { EntryData } from './models.ts'
import { KNOWN_CARDS, richCardOf } from './rich-card.ts'

const known = { schedule: 2 }
const data = (richCard: unknown): EntryData => ({ text: 'Every day at 8:00: the morning brief', final: true, richCard: richCard as EntryData['richCard'] })

test('a card of a type and version the app knows is drawn', () => {
  const card = { type: 'schedule', v: 2, ref: { agent: 'echo', id: 'brief' }, snapshot: { cron: '0 8 * * *' } }
  assert.deepEqual(richCardOf(data(card), known), card)
  assert.deepEqual(richCardOf(data({ type: 'schedule', v: 1 }), known), { type: 'schedule', v: 1 })
})

test('an unknown type or a newer version is the plain row', () => {
  assert.equal(richCardOf(data({ type: 'news', v: 1 }), known), null)
  assert.equal(richCardOf(data({ type: 'schedule', v: 3 }), known), null)
  assert.equal(richCardOf(data({ type: 'constructor', v: 1 }), known), null)
})

test('anything that is not a card is the plain row, never an error', () => {
  for (const bad of [undefined, null, 'schedule', 7, [], {}, { type: 'schedule' }, { type: 'schedule', v: '1' },
    { type: 'schedule', v: 0 }, { type: 'schedule', v: 1.5 }, { type: 'schedule', v: 1, ref: 'brief' },
    { type: 'schedule', v: 1, snapshot: [1] }, { type: 7, v: 1 }]) {
    assert.equal(richCardOf(data(bad), known), null, JSON.stringify(bad))
  }
})

test('the permission card is not a rich card', () => {
  assert.equal(richCardOf({ card: 'key' }, known), null)
})

test('this app draws Schedule, News and Plan cards of version 1', () => {
  assert.deepEqual(KNOWN_CARDS, { schedule: 1, news: 1, plan: 1 })
  assert.deepEqual(richCardOf(data({ type: 'schedule', v: 1 })), { type: 'schedule', v: 1 })
  assert.deepEqual(richCardOf(data({ type: 'news', v: 1 })), { type: 'news', v: 1 })
  assert.equal(richCardOf(data({ type: 'news', v: 2 })), null)
})
