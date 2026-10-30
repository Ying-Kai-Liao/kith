import assert from 'node:assert/strict'
import { test } from 'node:test'
import { ConversationBook, clampTurns, endLine, lateOf, lateReason, liveLine, parseConversation, waitText } from './conversation.ts'

const raw = (over: Record<string, unknown> = {}) => ({
  type: 'conversation', conversation: 'c1', chat: 'g1', lane: null, budget: 10, left: 10, state: 'open',
  endedWhy: null, current: null, waitUntil: null, opening: 'e1', ...over,
})
const conv = (over: Record<string, unknown> = {}) => parseConversation(raw(over))!

test('clampTurns keeps 1..100 and falls back to 10', () => {
  assert.equal(clampTurns(0), 1)
  assert.equal(clampTurns(-4), 1)
  assert.equal(clampTurns(101), 100)
  assert.equal(clampTurns(7.6), 8)
  assert.equal(clampTurns('25'), 25)
  assert.equal(clampTurns(''), 10)
  assert.equal(clampTurns(NaN), 10)
  assert.equal(clampTurns(undefined), 10)
})

test('a conversation reads with absent fields and refuses one without an id', () => {
  const c = parseConversation({ conversation: 'c1', chat: 'g1', budget: 5, left: 9, state: 'ended' })!
  assert.equal(c.lane, null)
  assert.equal(c.left, 5)
  assert.equal(c.endedWhy, null)
  assert.equal(c.current, null)
  assert.equal(c.waitUntil, null)
  assert.equal(c.opening, null)
  assert.equal(parseConversation({ chat: 'g1' }), null)
  assert.equal(parseConversation(null), null)
})

test('an unknown state reads as ended and an unknown reason as unknown', () => {
  const c = conv({ state: 'sleeping', endedWhy: 'tired' })
  assert.equal(c.state, 'ended')
  assert.equal(c.endedWhy, 'unknown')
  assert.equal(endLine(c), null)
})

test('end lines say how the run ended', () => {
  assert.deepEqual(endLine(conv({ state: 'ended', endedWhy: 'spent', left: 0 })), { text: 'Used all 10 turns', more: true })
  assert.deepEqual(endLine(conv({ state: 'ended', endedWhy: 'quiet', left: 4 })), { text: 'Worked it out · 6 of 10 turns', more: false })
  assert.deepEqual(endLine(conv({ state: 'stopped', endedWhy: 'stopped', left: 6 })), { text: 'You stopped it · 4 of 10 turns used', more: false })
  // A stop an older host recorded without its reason still reads as a stop.
  assert.equal(endLine(conv({ state: 'stopped', endedWhy: null, left: 6 }))?.text, 'You stopped it · 4 of 10 turns used')
  assert.equal(endLine(conv({ budget: 1, left: 0, state: 'ended', endedWhy: 'spent' }))?.text, 'Used all 1 turn')
})

test('an open run, an idle one, a replaced one and an old one without a reason have no end line', () => {
  assert.equal(endLine(conv()), null)
  assert.equal(endLine(conv({ state: 'ended', endedWhy: 'idle' })), null)
  assert.equal(endLine(conv({ state: 'ended', endedWhy: 'newer' })), null)
  assert.equal(endLine(conv({ state: 'ended', endedWhy: null })), null)
})

test('the live line names whose turn it is and how long a bot is waited for', () => {
  const now = 1_000_000
  const agent = liveLine(conv({ left: 6, current: { id: 'a', kind: 'agent', name: 'coder' } }), now)
  assert.equal(agent.who, 'coder')
  assert.equal(agent.left, '6 of 10 turns left')
  assert.equal(agent.fraction, 0.6)
  assert.equal(agent.wait, null)
  const bot = liveLine(conv({ left: 7, current: { id: 'b', kind: 'bot', name: 'Grok' }, waitUntil: now + 290_000 }), now)
  assert.equal(bot.wait, 'waits up to 5 min')
  assert.equal(waitText(1000), 'waits up to 1 min')
  assert.equal(liveLine(conv(), now).who, '')
})

test('events out of order keep the newest state of one conversation', () => {
  const book = new ConversationBook()
  assert.ok(book.apply(conv({ left: 8 })))
  assert.ok(book.apply(conv({ left: 5 })))
  // An older report of the same run changes nothing.
  assert.equal(book.apply(conv({ left: 7 })), false)
  assert.equal(book.inLane('g1', null)?.left, 5)
  assert.ok(book.apply(conv({ state: 'ended', endedWhy: 'quiet', left: 4 })))
  assert.equal(book.apply(conv({ left: 3 })), false)
  assert.equal(book.inLane('g1', null)?.state, 'ended')
  // The same end repeated with its reason beats the one without.
  assert.ok(book.apply(conv({ state: 'ended', endedWhy: 'quiet', left: 4 })) === false)
})

test('a new run takes the lane and the old run late end does not close it', () => {
  const book = new ConversationBook()
  book.apply(conv({ conversation: 'c1', left: 5 }))
  book.apply(conv({ conversation: 'c2', opening: 'e2' }))
  assert.equal(book.inLane('g1', null)?.id, 'c2')
  book.apply(conv({ conversation: 'c1', state: 'ended', endedWhy: 'newer', left: 5 }))
  const lane = book.inLane('g1', null)!
  assert.equal(lane.id, 'c2')
  assert.equal(lane.state, 'open')
  assert.equal(book.get('c1')?.endedWhy, 'newer')
})

test('lanes are separate and only ended lanes with a line are listed', () => {
  const book = new ConversationBook()
  book.apply(conv({ conversation: 'c1', state: 'ended', endedWhy: 'spent', left: 0 }))
  book.apply(conv({ conversation: 't1', lane: 'thread-1' }))
  assert.equal(book.inLane('g1', 'thread-1')?.id, 't1')
  assert.deepEqual(book.endedIn('g1').map((c) => c.id), ['c1'])
  assert.deepEqual(book.endedIn('other'), [])
})

test('late replies: a reason per lateWhy, none for an old late or an unprompted post', () => {
  assert.equal(lateReason('stopped'), 'Came after you stopped, so it starts nothing.')
  assert.ok(lateReason('newer'))
  assert.ok(lateReason('ended'))
  assert.ok(lateReason('noTurn'))
  assert.equal(lateReason(undefined), null)
  assert.equal(lateReason('somethingNew'), null)
  assert.deepEqual(lateOf({ late: true }), { reason: null })
  assert.deepEqual(lateOf({ late: true, lateWhy: 'stopped' }), { reason: 'Came after you stopped, so it starts nothing.' })
  assert.equal(lateOf({}), null)
  assert.equal(lateOf({ late: false }), null)
})
