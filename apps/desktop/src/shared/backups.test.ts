import assert from 'node:assert/strict'
import { test } from 'node:test'
import {
  addable, backAt, familyOf, hasClaudeAccount, isOut, loginCommand, mainChoices, mainEntry, mainSourceText, move, parseBackups,
  stateText, viaLabel, entryFor,
} from './backups.ts'

const sample = {
  global: ['codex', 'claude:work'],
  agents: { house: ['claude:work'] },
  entries: [
    { id: 'codex', harness: 'codex', account: null, kind: 'subscription', ready: false, why: 'not logged in', fix: 'codex login', outUntil: null, reason: null },
    { id: 'claude:work', harness: 'claude', account: 'work', kind: 'subscription', ready: true, why: null, fix: null, outUntil: 2_000_000, reason: 'rate_limit' },
    { id: 'codex:deep', harness: 'codex', account: 'deep', kind: 'subscription', ready: true },
    { bad: true },
  ],
}

test('the host answer parses and leaves out what it cannot read', () => {
  const b = parseBackups(sample)
  assert.deepEqual(b.global, ['codex', 'claude:work'])
  assert.deepEqual(b.agents, { house: ['claude:work'] })
  assert.equal(b.entries.length, 3)
  assert.equal(b.entries[1]!.outUntil, 2_000_000)
  assert.equal(b.entries[2]!.why, null)
})

test('garbage reads as an empty chain', () => {
  for (const raw of [null, 'x', [], {}, { global: 5, entries: 'no' }]) {
    const b = parseBackups(raw)
    assert.deepEqual([b.global, b.agents, b.entries], [[], {}, []])
  }
})

test('the badge says which entry answered, and nothing for the primary or old messages', () => {
  assert.equal(viaLabel('codex'), 'via codex')
  assert.equal(viaLabel('claude:work'), 'via claude:work')
  for (const none of [null, undefined, '', 5]) assert.equal(viaLabel(none), null)
})

test('state: ready, out until a time, or blocked with a reason', () => {
  const b = parseBackups(sample)
  const now = 1_000_000
  assert.equal(stateText(b.entries[1]!, now).tone, 'out')
  assert.match(stateText(b.entries[1]!, now).text, /^out of usage, back at /)
  assert.deepEqual(stateText(b.entries[0]!, now), { text: 'not logged in', tone: 'blocked' })
  assert.deepEqual(stateText(b.entries[2]!, now), { text: 'ready', tone: 'ok' })
  assert.equal(isOut(b.entries[1]!, 3_000_000), false, 'back once the time has passed')
  assert.match(backAt(now + 60_000, now), /^back at /)
})

test('an entry out with no reset time still reads as out only when the host sent a time', () => {
  const e = { ...parseBackups(sample).entries[2]!, outUntil: null, reason: 'rate_limit' }
  assert.equal(isOut(e), false)
})

test('move reorders and ignores a place that is not there', () => {
  assert.deepEqual(move(['a', 'b', 'c'], 2, 0), ['c', 'a', 'b'])
  assert.deepEqual(move(['a', 'b', 'c'], 0, 1), ['b', 'a', 'c'])
  assert.deepEqual(move(['a', 'b'], 0, 5), ['a', 'b'])
  assert.deepEqual(move(['a', 'b'], -1, 0), ['a', 'b'])
})

test('add offers ready entries first, then the plain providers, never what is in the order', () => {
  const b = parseBackups(sample)
  assert.deepEqual(addable(b, ['codex']), ['claude:work', 'codex:deep', 'claude'])
  assert.deepEqual(addable(parseBackups({}), []), ['codex', 'claude'])
})

test('the policy note is for extra Claude accounts only', () => {
  assert.equal(hasClaudeAccount(['codex', 'claude:work']), true)
  assert.equal(hasClaudeAccount(['codex', 'claude', 'claude-api:main']), false)
})

test('an entry the host has not described yet still has a name and a login command', () => {
  const e = entryFor(parseBackups({}), 'codex')
  assert.equal(e.harness, 'codex')
  assert.equal(loginCommand(e), 'codex login')
  assert.equal(loginCommand(entryFor(parseBackups({}), 'claude:x')), null)
})

test('the main accounts parse above the chain; an old host reads as none', () => {
  const b = parseBackups({
    ...sample,
    main: {
      global: 'claude:work',
      agents: {
        house: { id: 'claude:work', source: 'global', harness: 'claude', ready: true },
        deep: { id: 'codex:deep', source: 'agent', harness: 'codex', ready: false, why: 'not logged in', fix: 'kith account login codex deep' },
        bad: { source: 'agent' },
        worse: 5,
      },
    },
  })
  assert.equal(b.main.global, 'claude:work')
  assert.deepEqual(Object.keys(b.main.agents), ['house', 'deep'])
  assert.equal(b.main.agents.house!.why, null)
  assert.equal(b.main.agents.deep!.fix, 'kith account login codex deep')
  assert.equal(mainSourceText(b.main.agents.house!), "the computer's main account")
  assert.equal(mainSourceText(b.main.agents.deep!), "this bot's own choice")
  assert.equal(mainSourceText({ ...b.main.agents.deep!, source: 'default' }), 'the default login')
  assert.deepEqual(parseBackups(sample).main, { global: null, agents: {} })
})

test('a main account is of the agent harness: an API key is a Claude one, ready first', () => {
  const b = parseBackups({ ...sample, entries: [...sample.entries, { id: 'claude-api:main', harness: 'claude', kind: 'api-key', ready: true }] })
  assert.equal(familyOf('claude-api:main'), 'claude')
  assert.equal(familyOf('codex:deep'), 'codex')
  assert.deepEqual(mainChoices(b, 'claude'), ['claude:work', 'claude-api:main', 'claude'])
  assert.deepEqual(mainChoices(b, 'codex'), ['codex:deep', 'codex'])
  assert.equal(mainChoices(b, null).length, 5)
})

test('the main entry takes its state from the main row, which knows the agent', () => {
  const b = parseBackups(sample)
  const e = mainEntry(b, { id: 'claude:work', source: 'agent', harness: 'claude', ready: false, why: 'logged out', fix: 'claude auth login' })
  assert.equal(stateText(e).text, 'logged out')
  assert.equal(e.outUntil, 2_000_000)
})
