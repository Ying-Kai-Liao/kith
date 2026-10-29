import assert from 'node:assert/strict'
import { test } from 'node:test'
import { contextFill, contextLabel, contextLine } from './context-hint.ts'

test('context label: the number as is, also past 100', () => {
  assert.equal(contextLabel(31), '31% context')
  assert.equal(contextLabel(41.6), '42% context')
  assert.equal(contextLabel(120), '120% context')
})

test('context label: nothing when absent or not a number', () => {
  for (const v of [undefined, null, '42', NaN]) assert.equal(contextLabel(v), null)
})

test('context fill: clamped to 0..1, null without a number', () => {
  assert.equal(contextFill(25), 0.25)
  assert.equal(contextFill(150), 1)
  assert.equal(contextFill(-5), 0)
  assert.equal(contextFill(undefined), null)
})

test('context line: an old reply without a percent shows only the time', () => {
  assert.deepEqual(contextLine('3:27 PM', undefined), { time: '3:27 PM', fill: null, label: null })
  assert.deepEqual(contextLine('3:27 PM', 31), { time: '3:27 PM', fill: 0.31, label: '31% context' })
})
