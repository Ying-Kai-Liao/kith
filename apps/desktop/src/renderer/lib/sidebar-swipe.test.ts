import assert from 'node:assert/strict'
import { test } from 'node:test'
import { swipeTracker } from './sidebar-swipe.ts'

test('a left swipe past the threshold goes to Board, once, through its momentum', () => {
  const feed = swipeTracker()
  assert.equal(feed(20, 1, 0), null)
  assert.equal(feed(30, 1, 16), null)
  assert.equal(feed(30, 0, 32), 'board')
  assert.equal(feed(40, 0, 48), null)
  assert.equal(feed(5, 0, 64), null)
})

test('a right swipe goes to Crew', () => {
  assert.equal(swipeTracker()(-70, 2, 0), 'crew')
})

test('vertical scrolling never switches', () => {
  const feed = swipeTracker()
  for (let t = 0; t < 20; t++) assert.equal(feed(10, 40, t * 16), null)
})

test('a new gesture after a pause switches again', () => {
  const feed = swipeTracker()
  assert.equal(feed(80, 0, 0), 'board')
  assert.equal(feed(-80, 0, 1000), 'crew')
})
