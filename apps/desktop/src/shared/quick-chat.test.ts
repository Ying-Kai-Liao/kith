import assert from 'node:assert/strict'
import { test } from 'node:test'
import { keepNameError, QUICK_ENGINES, quickChatBot, quickEngine } from './quick-chat.ts'
import { normalizeBot } from './models.ts'

const bot = (over: Record<string, unknown>) => normalizeBot({ id: 'b', ...over } as never)

test("the engine picks send nothing when both are left at scratch's own", () => {
  assert.equal(quickEngine('', ''), null)
  assert.equal(quickEngine(' ', ' '), null)
  assert.deepEqual(quickEngine('codex', ''), { engine: 'codex' })
  assert.deepEqual(quickEngine('', 'opus'), { engine: 'claude', engineModel: 'opus' }, 'a model alone means Claude')
  assert.deepEqual(QUICK_ENGINES.map((e) => e.id), ['', 'claude', 'codex'])
})

test('Quick chat talks to the built-in scratch bot only', () => {
  const builtin = bot({ id: 'q', hqAgent: 'scratch', builtin: true })
  assert.equal(quickChatBot([bot({ id: 'a' }), builtin])?.id, 'q')
  assert.equal(quickChatBot([bot({ id: 'mine', hqAgent: 'scratch' })]), undefined, "a user's own scratch agent is an ordinary bot")
  assert.equal(quickChatBot([bot({ id: 'a', hqAgent: 'house' })]), undefined, 'a kernel without scratch: no Quick chat')
  assert.equal(quickChatBot([bot({ id: 'q', hqAgent: 'scratch', builtin: true, hidden: true })]), undefined)
})

test('the builtin flag reads from the host and is absent otherwise', () => {
  assert.equal(bot({ builtin: true }).builtin, true)
  assert.equal(bot({}).builtin, undefined)
  assert.equal(bot({ builtin: false }).builtin, undefined)
})

test('a kept agent needs a name without spaces or a leading dash', () => {
  assert.equal(keepNameError(''), 'Name the agent.')
  assert.ok(keepNameError('two words'))
  assert.ok(keepNameError('-H'))
  assert.ok(keepNameError('x'.repeat(65)))
  assert.equal(keepNameError('trip-planner'), null)
})
