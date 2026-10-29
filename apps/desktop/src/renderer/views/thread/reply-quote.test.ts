import assert from 'node:assert/strict'
import { test } from 'node:test'
import { quoteOf, ReplyQuotes, withQuote } from './reply-quote.ts'

test('the quote is the first line that says something, cut short', () => {
  assert.equal(quoteOf('\n  Ship it today  \nthen rest'), 'Ship it today')
  assert.equal(quoteOf('   '), null)
  assert.equal(quoteOf(undefined), null)
  assert.equal(quoteOf('x'.repeat(300))!.length, 160)
})

test('a reply starts with the quote as a blockquote and a blank line', () => {
  assert.equal(withQuote('yes, do that', 'Ship it today'), '> Ship it today\n\nyes, do that')
  assert.equal(withQuote('plain', null), 'plain')
})

test('a chat holds one quote at a time and tells its listeners', () => {
  const q = new ReplyQuotes()
  let calls = 0
  q.subscribe(() => calls++)
  q.set('a', 'one')
  q.set('a', 'two')
  assert.equal(q.get('a'), 'two')
  assert.equal(q.get('b'), null)
  q.set('a', null)
  q.set('a', null)
  assert.equal(q.get('a'), null)
  assert.equal(calls, 3, 'clearing nothing tells no one')
})
