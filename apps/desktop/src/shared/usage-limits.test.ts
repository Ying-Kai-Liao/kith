import assert from 'node:assert/strict'
import { test } from 'node:test'
import type { Entry } from './models.ts'
import {
  limitCardModel,
  limitForecast,
  limitOutcome,
  limitQuestion,
  modelLine,
  money,
  ofLimit,
  parseAgentPeople,
  parseBotsCap,
  parseUsageReport,
  personSpend,
} from './usage-limits.ts'

const NOW = new Date('2026-10-26T14:02:00')

const card = (data: Entry['data']): Entry => ({ id: 'p1', seq: 0, botId: 'architect', threadId: null, rev: 1, kind: 'permission', turn: 1, data, createdAt: NOW.getTime(), updatedAt: NOW.getTime() })

const agentCard = (extra: Entry['data'] = {}) =>
  card({
    status: 'pending',
    toolKind: 'limit',
    agent: 'architect',
    title: 'This run is waiting: architect has used its $300 for October.',
    options: [
      { optionId: 'allow', name: 'Allow this run', kind: 'limit' },
      { optionId: 'raise', name: 'Raise to $400', kind: 'limit' },
      { optionId: 'stop', name: 'Stop', kind: 'limit' },
    ],
    limit: { kind: 'agent', agent: 'architect', spent: 300.4, limit: 300, at: 'ask', month: 'October', perRun: 2.92, raiseTo: 400 },
    ...extra,
  } as Entry['data'])

test('money: whole dollars with commas from $10, cents below or when asked', () => {
  assert.equal(money(6667.2), '$6,667')
  assert.equal(money(2.92), '$2.92')
  assert.equal(money(289.01, true), '$289.01')
  assert.equal(ofLimit(300.4, 300, true), '$300.40 of $300')
})

test('the usage report reads the ledger as it is, with no limit as null', () => {
  const r = parseUsageReport({ period: 'month', label: 'October', since: '2026-10-01', prices: 'api', total: 6667, agents: 910, own: 5757, rows: [{ agent: 'architect', cost: 289.01, runs: 99, perRun: 2.92, limit: 300, at: 'ask', month: 289.01 }, { agent: 'coder', cost: 12, runs: 4, perRun: 3, limit: null, at: null, month: null }] })
  assert.equal(r.label, 'October')
  assert.equal(r.rows[0]!.limit, 300)
  assert.equal(r.rows[1]!.limit, null)
  assert.equal(r.own, 5757)
})

test('the forecast says when the chosen limit runs out and how far the next one lasts', () => {
  const f = { perDay: 72, since: '2026-10-23', text: '', reach: { '300': '2026-10-26', '500': '2026-10-29' } }
  assert.equal(limitForecast(f, 289.01, 300, NOW), 'At about $72 a day, $300 runs out today and $500 lasts to about Oct 29.')
  assert.equal(limitForecast({ ...f, perDay: 0.07, reach: {} }, 35.82, 100, NOW), 'At about $0.07 a day, $100 lasts the month.')
  assert.equal(limitForecast(f, 289.01, null, NOW), 'At about $72 a day, no limit: runs never wait for one.')
  assert.equal(limitForecast(null, 0, null, NOW), 'No limit: runs never wait for one.')
})

test('model and effort name the model most of the cost ran on', () => {
  assert.equal(modelLine([{ model: 'claude-sonnet-5-1', effort: 'medium', cost: 3, runs: 9 }, { model: 'claude-opus-5-5', effort: 'high', cost: 280, runs: 90 }]), 'Opus 5.5 · high')
  assert.equal(modelLine([]), '—')
})

test("an agent's limit card: the run waits, the message is kept, allow first", () => {
  const m = limitCardModel(agentCard())!
  assert.equal(m.heading, 'architect · Monthly limit')
  assert.equal(m.title, 'This run is waiting: architect has used its $300 for October.')
  assert.equal(m.left, 'About $3 a run')
  assert.equal(m.right, '$300.40 of $300')
  assert.equal(m.fullness, 1)
  assert.deepEqual(m.options.map((o) => [o.label, o.tone]), [['Allow this run', 'ink'], ['Raise to $400', 'pair'], ['Stop', 'plain']])
  assert.equal(m.footer, 'Your message is kept. Nothing runs until you answer.')
})

test('on Board the card says one run is waiting, without the chat footer', () => {
  const m = limitCardModel(agentCard(), undefined, true)!
  assert.equal(m.title, 'architect used its $300 for October. One run is waiting.')
  assert.equal(m.footer, null)
  assert.equal(limitQuestion(agentCard()), 'architect used its $300 for October. One run is waiting.')
})

test("a guest's card names them and the agent, and its answers say what happened", () => {
  const guest = card({
    status: 'pending',
    toolKind: 'limit',
    approver: 'owner',
    agent: 'video',
    person: 'mia',
    personName: 'Mia',
    title: "Mia reached their $20 for October. Their last message wasn't run.",
    options: [
      { optionId: 'add', name: 'Add $10', kind: 'limit' },
      { optionId: 'raise', name: 'Raise to $40', kind: 'limit' },
      { optionId: 'leave', name: 'Leave it', kind: 'limit' },
    ],
    limit: { kind: 'person', agent: 'video', person: 'mia', spent: 20.1, limit: 20, month: 'October', addTo: 30, raiseTo: 40 },
  } as Entry['data'])
  const m = limitCardModel(guest)!
  assert.equal(m.heading, 'Mia · limit on video')
  assert.equal(m.right, '$20.10 of $20')
  assert.equal(m.footer, null)
  assert.equal(limitOutcome({ ...guest, data: { ...guest.data, status: 'answered', selected: 'add' } }), 'Added $10')
  assert.equal(limitOutcome({ ...agentCard(), data: { ...agentCard().data, status: 'answered', selected: 'raise' } }), 'Raised to $400')
  assert.equal(limitOutcome({ ...agentCard(), data: { ...agentCard().data, status: 'answered', selected: 'stop' } }), 'Stopped · your message is kept')
})

test("the owner's bots' card speaks for all of them, and the cap reads as none when off", () => {
  const bots = card({
    status: 'pending',
    toolKind: 'limit',
    approver: 'owner',
    agent: 'house',
    title: "Your bots reached their $50 for October. Rex's last message wasn't run.",
    options: [{ optionId: 'add', name: 'Add $10', kind: 'limit' }, { optionId: 'leave', name: 'Leave it', kind: 'limit' }],
    limit: { kind: 'bots', agent: 'house', person: 'bot-k', spent: 50.2, limit: 50, month: 'October', addTo: 60, raiseTo: 100 },
  } as Entry['data'])
  const m = limitCardModel(bots)!
  assert.equal(m.heading, 'Your bots · monthly cap')
  assert.equal(m.right, '$50.20 of $50')
  assert.equal(limitQuestion(bots), 'Your bots reached their $50 for October.')
  assert.deepEqual(parseBotsCap({ month: 'October', cap: null, spent: 3, bots: { 'bot-k': 3 } }), { month: 'October', cap: null, spent: 3 })
})

test("a person's spend reads against their budget; none is no limit", () => {
  const p = parseAgentPeople({ agent: 'video', month: '2026-10', people: { owner: 35.82, mia: 20.1 }, budgets: { mia: 20, ren: null } })
  assert.deepEqual(personSpend(p, 'mia'), { spent: 20.1, budget: 20 })
  assert.deepEqual(personSpend(p, 'ren'), { spent: 0, budget: null })
})

test('a card without a limit is not a limit card', () => {
  assert.equal(limitCardModel(card({ status: 'pending', title: 'npm test' })), null)
})
