// The host RPC contract (serve/contract/host/contract.json) as desktop and web read it: the generated file is the
// contract's, and the member and bot answers of today's host (fixtures/, written by the host's own test from real calls), an
// older (compat/) and a newer (unknown/) host all read.

import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { readdirSync, readFileSync } from 'node:fs'
import { test } from 'node:test'
import { BOT_METHODS, BOT_PENDING_METHODS, CONTRACT_SHA256, check, MEMBER_METHODS, METHODS, receive, type MethodName, type TypeName } from './host-contract.gen.ts'
import { parseGranted } from './mac-role.ts'
import { asEntry, normalizeBot } from './models.ts'

const dir = new URL('../../../../serve/contract/host/', import.meta.url)
const fixtures = (sub: '' | 'compat/' | 'unknown/') =>
  readdirSync(new URL(`fixtures/${sub}`, dir))
    .filter((f) => f.endsWith('.json'))
    .map((f) => ({
      f,
      ...(JSON.parse(readFileSync(new URL(`fixtures/${sub}${f}`, dir), 'utf8')) as { method: MethodName; type: TypeName; body: unknown }),
    }))
const one = (sub: 'compat/' | 'unknown/', f: string) => {
  const found = fixtures(sub).find((x) => x.f === f)
  assert.ok(found, f)
  return found
}

test('host-contract.gen.ts is made from contract.json as it is now (else: node serve/contract/gen.mjs)', () => {
  assert.equal(createHash('sha256').update(readFileSync(new URL('contract.json', dir))).digest('hex'), CONTRACT_SHA256)
})

test("today's, an older and a newer host's answers all read", () => {
  for (const sub of ['', 'compat/', 'unknown/'] as const) {
    const all = fixtures(sub)
    assert.ok(all.length > 0, sub)
    for (const { f, method, type, body } of all) {
      assert.equal(METHODS[method].result, type, `${sub}${f}`)
      assert.deepEqual(check(type, body, 'consumer'), [], `${sub}${f}`)
    }
  }
  // The host's own answers are what a producer may send; a newer host's words are not this version's.
  for (const { f, type, body } of fixtures('')) assert.deepEqual(check(type, body, 'producer'), [], f)
  for (const { f, type, body } of fixtures('unknown/')) assert.notDeepEqual(check(type, body, 'producer'), [], f)
})

test("a newer host's words read as the contract says", () => {
  const started = one('unknown/', 'start-chat-new-words.json').body
  const { bot } = receive('BotAnswer', started)
  assert.deepEqual([bot.kind, bot.status, bot.permission], ['agent', 'idle', 'unknown'])
  assert.ok('newField' in bot, 'fields this version does not know are passed through, not refused')
  // The app's own Bot (startChat stays on it) reads the status the same way.
  assert.equal(normalizeBot((started as { bot: { id: string } }).bot).status, 'idle')
  const history = receive('Entries', one('unknown/', 'history-new-kind.json').body)
  assert.equal(asEntry(history.entries[0]).kind, 'unknown')
  const hello = one('unknown/', 'hello-new-words.json').body as { grantedAgents: unknown; device: unknown }
  assert.deepEqual(parseGranted(hello.grantedAgents)?.map((g) => g.role), ['guest'], "a role this version doesn't know is the least")
  assert.equal(receive('Hello', hello).device, 'desktop')
  const [guest] = receive('BotGuests', one('unknown/', 'my-bots-new-words.json').body).bots
  assert.deepEqual([guest.provider, guest.state], ['other', 'unknown'])
  assert.equal(receive('ThreadProjects', one('unknown/', 'thread-projects-new-by.json').body).by, 'inferred')
  assert.equal(receive('Citation', one('unknown/', 'citation-new-error.json').body).error, 'not_shared')
})

test("an older host's answers leave out what it didn't have", () => {
  assert.equal(parseGranted((one('compat/', 'hello-before-granted-agents.json').body as { grantedAgents?: unknown }).grantedAgents), null)
  const { bot } = receive('BotAnswer', one('compat/', 'start-chat-before-people.json').body)
  assert.equal(bot.person, undefined)
})

test('a left-out list reads as empty, and the member and bot lists name contract methods', () => {
  assert.deepEqual(receive('Entries', {}).entries, [])
  for (const m of MEMBER_METHODS) assert.ok(METHODS[m].who.includes('member'), m)
  for (const m of BOT_METHODS) assert.ok(METHODS[m].who.includes('bot'), m)
  for (const m of BOT_PENDING_METHODS) assert.ok(METHODS[m].who.includes('botPending'), m)
})

test("a newer host's words to a bot read as the contract says", () => {
  const thread = receive('BotThread', one('unknown/', 'bot-thread-new-words.json').body)
  assert.deepEqual([thread.entries[0].kind, thread.state], ['unknown', 'running'])
  const me = receive('BotSelf', one('unknown/', 'bot-status-new-words.json').body)
  assert.deepEqual([me.state, me.agents[0].role], ['unknown', 'guest'])
})
