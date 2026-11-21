import assert from 'node:assert/strict'
import { test } from 'node:test'
import { madeButFailed, newAgentDescriptionError, newAgentNameError, newAgentParams, parseKernelStatus, prefillOf, parseNewAgentResult, type NewAgentForm } from './new-agent.ts'

const form = (over: Partial<NewAgentForm> = {}): NewAgentForm => ({
  name: ' trip-planner ', description: ' Plans trips ', harness: '', model: '', effort: 'default', ...over,
})

test('names follow the host rules', () => {
  assert.equal(newAgentNameError('trip-planner'), null)
  assert.equal(newAgentNameError('a1'), null)
  assert.ok(newAgentNameError(''))
  assert.ok(newAgentNameError('a'), 'too short')
  assert.ok(newAgentNameError('a'.repeat(32)), 'too long')
  assert.equal(newAgentNameError('a'.repeat(31)), null)
  assert.ok(newAgentNameError('1abc'), 'starts with a letter')
  assert.ok(newAgentNameError('-abc'))
  assert.ok(newAgentNameError('Trip'), 'lowercase only')
  assert.ok(newAgentNameError('trip planner'))
  assert.ok(newAgentNameError('kith'))
})

test('descriptions are one plain line', () => {
  assert.equal(newAgentDescriptionError('Plans trips'), null)
  assert.ok(newAgentDescriptionError('  '))
  assert.ok(newAgentDescriptionError('a\nb'))
  assert.ok(newAgentDescriptionError('-x'))
  assert.ok(newAgentDescriptionError('Does this: that'))
  assert.equal(newAgentDescriptionError('Does this:that'), null)
})

test('params carry only what was picked', () => {
  assert.deepEqual(newAgentParams(form()), { name: 'trip-planner', description: 'Plans trips' })
  assert.deepEqual(newAgentParams(form({ harness: 'codex', model: ' gpt ', effort: 'high' })), {
    name: 'trip-planner', description: 'Plans trips', harness: 'codex', model: 'gpt', effort: 'high',
  })
})

test('the answer maps with a missing bot', () => {
  assert.deepEqual(parseNewAgentResult({ agent: 'a', botId: null, bot: null }), { agent: 'a', botId: null, bot: null })
  const r = parseNewAgentResult<{ id: string }>({ agent: 'a', botId: 'x', bot: { id: 'x' } })
  assert.equal(r.botId, 'x')
  assert.equal(r.bot?.id, 'x')
  assert.equal(parseNewAgentResult(undefined).agent, '')
})

test('kernel status and the made-but error', () => {
  assert.deepEqual(parseKernelStatus({ ok: false, bin: null, problem: 'uv is gone', hint: 'Run kith doctor' }), { ok: false, bin: null, problem: 'uv is gone', hint: 'Run kith doctor' })
  assert.equal(parseKernelStatus({}).ok, false)
  assert.equal(parseKernelStatus({ ok: true, bin: '/x/kith' }).ok, true)
  assert.ok(madeButFailed("trip was made, but its effort couldn't be set: no", 'trip'))
  assert.ok(!madeButFailed('name taken', 'trip'))
})

test('a flagged row prefills the form: name, one-line description, a kith engine and its model', () => {
  assert.deepEqual(prefillOf({ name: 'Trip', description: '\nPlans trips: books them.\nNever pays.', backend: 'codex', model: 'gpt-5' }), {
    name: 'Trip', description: 'Plans trips - books them.', harness: 'codex', model: 'gpt-5',
  })
  assert.deepEqual(prefillOf({ name: 'Odd', description: '', backend: 'gemini', model: 'x' }), { name: 'Odd', description: '', harness: '', model: '' }, 'an engine kith cannot run stays on its default')
  assert.equal(newAgentDescriptionError(prefillOf({ name: 'a', description: '- a: b', backend: 'claude' }).description), null)
})
