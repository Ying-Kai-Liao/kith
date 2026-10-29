import assert from 'node:assert/strict'
import { test } from 'node:test'
import type { RichCard } from './rich-card.ts'
import { newsView } from './news-card.ts'

const at = (day: number, h: number, m: number) => new Date(2026, 9, day, h, m).getTime()
const row = (n: number) => ({ title: `Item ${n}`, line: 'One line.', source: n === 1 ? 'BOM' : null, domain: 'bom.gov.au', url: `https://bom.gov.au/${n}` })
const card = (rows: unknown[]): RichCard => ({
  type: 'news', v: 1, ref: { agent: 'personal', job: 'morning-brief' },
  snapshot: { name: 'Morning brief', every: 'Every day at 7:00', lead: 'Five worth your time today.', tail: 'Want the long read summarised?', rows },
})

test('the day it came: the header, three rows and "Show N more"', () => {
  const v = newsView(card([1, 2, 3, 4, 5].map(row)), { postedAt: at(25, 7, 0), now: at(25, 9, 0) })
  assert.equal(v.title, 'Morning brief · Sun 25 Oct')
  assert.equal(v.every, 'every day 7:00')
  assert.equal(v.lead, 'Five worth your time today.')
  assert.equal(v.tail, 'Want the long read summarised?')
  assert.deepEqual(v.shown.map((r) => r.title), ['Item 1', 'Item 2', 'Item 3'])
  assert.equal(v.more, 2)
  assert.equal(v.fold, null)
  assert.equal(newsView(card([1, 2, 3, 4, 5].map(row)), { postedAt: at(25, 7, 0), now: at(25, 9, 0), expanded: true }).more, 0)
})

test('after its day it folds to one line, and opened shows every item', () => {
  const rows = [1, 2, 3, 4, 5, 6].map(row)
  assert.equal(newsView(card(rows), { postedAt: at(24, 7, 0), now: at(25, 9, 0) }).fold, 'Morning brief · Sat · 6 items')
  const opened = newsView(card(rows), { postedAt: at(24, 7, 0), now: at(25, 9, 0), opened: true })
  assert.equal(opened.fold, null)
  assert.equal(opened.shown.length, 6)
})

test('rows without a title or a web link are left out; a missing schedule is null', () => {
  const v = newsView({ type: 'news', v: 1, snapshot: { name: 'Brief', rows: [row(1), { title: 'x', url: 'javascript:alert(1)' }, { url: 'https://a.b' }, 'no'] } }, { postedAt: at(25, 7, 0), now: at(25, 8, 0) })
  assert.deepEqual(v.rows.map((r) => r.title), ['Item 1'])
  assert.equal(v.every, null)
})
