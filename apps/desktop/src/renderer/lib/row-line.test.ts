import assert from 'node:assert/strict'
import test from 'node:test'
import { rowLine } from './row-line.ts'

test('the row shows the summary only when the setting is on and one is visible', () => {
  const base = { lastMessage: 'last words', lastSummary: 'A summary.', rowShowsSummary: true }
  assert.equal(rowLine(base), 'A summary.')
  assert.equal(rowLine({ ...base, rowShowsSummary: false }), 'last words', 'setting off')
  assert.equal(rowLine({ ...base, lastSummary: null }), 'last words', 'no summary yet, or hidden from this viewer')
  assert.equal(rowLine({ ...base, lastSummary: '  ' }), 'last words')
  assert.equal(rowLine({ rowShowsSummary: true, lastSummary: null, lastMessage: null }), null)
})
