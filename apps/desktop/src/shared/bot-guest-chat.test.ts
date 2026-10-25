import assert from 'node:assert/strict'
import { test } from 'node:test'
import { botGuestReadOnlyLine, botGuestTag, botOriginLabel, isBotGuestChat, nestBotGuestChats } from './bot-guest-chat.ts'
import { normalizeBot } from './models.ts'

const guest = { botId: 'bg1', name: 'Rex', provider: 'grok', ownerPerson: null, ownerName: null }

test('botGuest parses on the bot, and is absent on others and from old hosts', () => {
  const b = normalizeBot({ id: 'c', botGuest: { ...guest, ownerPerson: 'p1', ownerName: 'Sam' } as never })
  assert.deepEqual(b.botGuest, { ...guest, ownerPerson: 'p1', ownerName: 'Sam' })
  assert.equal(normalizeBot({ id: 'c' }).botGuest, undefined)
  assert.equal(normalizeBot({ id: 'c', botGuest: { name: 'x' } as never }).botGuest, undefined, 'no botId, no guest')
  assert.equal(isBotGuestChat(b), true)
  assert.equal(isBotGuestChat(normalizeBot({ id: 'd' })), false)
})

test('the tag names the provider and whose bot it is', () => {
  assert.equal(botGuestTag({ ...guest, ownerName: 'Sam' }), "from Grok · Sam's Rex")
  assert.equal(botGuestTag(guest), 'from Grok · Your Rex')
  assert.equal(botGuestTag({ ...guest, provider: '' }), 'from a bot · Your Rex')
})

test('the read-only line says who asks', () => {
  assert.equal(botGuestReadOnlyLine(guest, 'personal'), 'Rex asks personal from Grok. You can read along here.')
})

test('a thread started by a bot is labelled from the host, else from originBot', () => {
  assert.equal(botOriginLabel({ originLabel: "from Grok · Kai's Grok" }), "from Grok · Kai's Grok")
  assert.equal(botOriginLabel({ originBot: { provider: 'grok', name: 'Rex' } }), 'from Grok · Rex')
  assert.equal(botOriginLabel({}), undefined)
})

test('a bot-guest chat sits right after its agent, once', () => {
  const a = { id: 'a', hqAgent: 'personal' }
  const x = { id: 'x', hqAgent: 'coder' }
  const g = { id: 'g', hqAgent: 'personal', botGuest: guest }
  const orphan = { id: 'o', hqAgent: 'gone', botGuest: guest }
  assert.deepEqual(nestBotGuestChats([g, a, x, orphan]).map((b) => b.id), ['a', 'g', 'x', 'o'])
  assert.deepEqual(nestBotGuestChats([a, x]).map((b) => b.id), ['a', 'x'])
})
