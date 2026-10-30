import assert from 'node:assert/strict'
import { test } from 'node:test'
import { addableBots, botRowLine, cannotJoinWhy } from './room-join.ts'

const bot = (over: Record<string, unknown> = {}) => ({ botId: 'b1', name: 'Grok', provider: 'grok', state: 'active' as const, ownerPerson: null, ownerName: null, ...over })

test('only the owner\'s own active bot can join; every other says why', () => {
  assert.equal(cannotJoinWhy(bot()), null)
  assert.equal(cannotJoinWhy(bot({ state: 'pending' })), 'Waiting for you to let it in')
  assert.equal(cannotJoinWhy(bot({ state: 'joining' })), "Hasn't joined yet")
  assert.equal(cannotJoinWhy(bot({ state: 'denied' })), 'You turned it away')
  assert.equal(cannotJoinWhy(bot({ state: 'revoked' })), 'Removed')
  assert.match(cannotJoinWhy(bot({ ownerPerson: 'p1', ownerName: 'Sam' }))!, /^Sam's bot · only your own bots join your rooms$/)
})

test('the member row says whose bot it is and where it runs', () => {
  assert.equal(botRowLine(bot()), 'Your bot · runs on Grok, outside this Mac')
  assert.equal(botRowLine(bot({ provider: '' })), 'Your bot · runs outside this Mac')
  assert.equal(botRowLine(null), 'Your bot · runs outside this Mac')
})

test('the Add list puts joinable bots first and leaves out the ones already in the room', () => {
  const rows = addableBots([bot({ botId: 'm', name: 'Muse', state: 'pending' }), bot({ botId: 'g' }), bot({ botId: 'in' })], ['in'])
  assert.deepEqual(rows.map((r) => r.bot.botId), ['g', 'm'])
  assert.equal(rows[0]!.why, null)
})
