import assert from 'node:assert/strict'
import { test } from 'node:test'
import { DictationTranscript, joinDictated } from './dictation-transcript.ts'

function feed(results: [string, boolean][]) {
  const t = new DictationTranscript()
  for (const [said, ends] of results) t.heard(said, ends)
  return t
}

test('the running segment replaces itself as it is corrected', () => {
  assert.equal(feed([['I', false], ['I scream', false], ['Ice cream is good', false]]).text, 'Ice cream is good')
})

test('a segment that ends is kept when the next one starts', () => {
  const t = feed([['Hello', false], ['Hello world.', true], ['Again', false], ['Again and again', false]])
  assert.equal(t.text, 'Hello world. Again and again')
})

test('a final that repeats the segment the pause already ended adds nothing', () => {
  assert.equal(feed([['Hello world', true], ['Hello world', true]]).text, 'Hello world')
})

test('a recognizer that goes on after a segment end without starting over keeps the words once', () => {
  const t = feed([['Hello world', true], ['Hello world how', false], ['Hello world how are you', true], ['Hello world how are you fine', false]])
  assert.equal(t.text, 'Hello world how are you fine')
})

test('a transcript that starts over after a pause without saying so keeps the earlier words', () => {
  const t = feed([['Buy', false], ['Buy milk and bread', false], ['Then', false], ['Then call mom', false]])
  assert.equal(t.text, 'Buy milk and bread Then call mom')
})

test('a restart keeps the running words and the next recognition goes after them', () => {
  const t = feed([['Buy milk', false]])
  t.restart()
  t.heard('Buy', false)
  t.heard('Buy bread', false)
  assert.equal(t.text, 'Buy milk Buy bread')
  t.heard('Buy bread.', true)
  t.restart()
  t.heard('Done', false)
  assert.equal(t.text, 'Buy milk Buy bread. Done')
})

test('dictated words go after typed text with one space', () => {
  assert.equal(joinDictated('Note:', 'hello'), 'Note: hello')
  assert.equal(joinDictated('Note: ', 'hello'), 'Note: hello')
  assert.equal(joinDictated('  ', 'hello'), 'hello')
  assert.equal(joinDictated('Note:', ''), 'Note:')
})
