import assert from 'node:assert/strict'
import { test } from 'node:test'
import { normalizeBot } from '../../../shared/models.ts'
import { builtInCommand, returnRuns, slashCommands, slashQuery } from './slash-commands.ts'

const installed = [
  { id: 'pdf', name: 'PDF', description: 'PDFs' },
  { id: 'code-review', name: 'Code Review', description: '' },
  { id: 'xlsx', name: 'xlsx', description: '' },
]

test('a leading slash opens the list until the name ends', () => {
  assert.equal(slashQuery('/'), '')
  assert.equal(slashQuery('/pd'), 'pd')
  assert.equal(slashQuery('/pdf merge'), null)
  assert.equal(slashQuery('hi /pdf'), null)
})

test('built-in commands come first, then only the bot\'s own skills, by name or title', () => {
  const bot = normalizeBot({ id: 'b', skills: ['pdf', 'code-review'], fastModeSupport: 'offered' })
  assert.deepEqual(slashCommands(bot, '', installed).map((c) => c.name), ['fast', 'pdf', 'code-review'])
  assert.deepEqual(slashCommands(bot, 'review', installed).map((c) => c.name), ['code-review'])
  assert.deepEqual(slashCommands(bot, 'new', installed), [], 'a new session is in the bot\'s menu')
  assert.deepEqual(slashCommands(normalizeBot({ id: 'b', skills: ['pdf'] }), 'xl', installed), [])
  assert.deepEqual(slashCommands(normalizeBot({ id: 'b' }), '', []).map((c) => c.name), ['fast'], 'offered until the agent says otherwise')
  assert.deepEqual(slashCommands(normalizeBot({ id: 'b', fastModeSupport: 'unsupported' }), '', []), [], 'no Fast mode on offer')
  assert.deepEqual(slashCommands(normalizeBot({ id: 'g', kind: 'group', fastModeSupport: 'offered' }), '', installed), [])
})

test('a kith agent has no /fast: the CLI has no Fast mode setting, so it is not offered or run', () => {
  const bot = normalizeBot({ id: 'b', hqAgent: 'house', fastModeSupport: 'offered' })
  assert.deepEqual(slashCommands(bot, '', []), [])
  assert.equal(builtInCommand(bot, '/fast'), null)
})

test('sending just a built-in command runs it; anything else is a message', () => {
  const bot = normalizeBot({ id: 'b', fastModeSupport: 'offered' })
  assert.equal(builtInCommand(bot, '/fast ')?.action.kind, 'fast')
  assert.equal(builtInCommand(bot, '/new'), null, 'reaches the agent as typed')
  assert.equal(builtInCommand(bot, '/fast please'), null)
  assert.equal(builtInCommand(normalizeBot({ id: 'b', fastModeSupport: 'unsupported' }), '/fast'), null, 'without Fast mode it reaches the agent')
})

test('Return runs the highlighted command, but sends a skill already typed out', () => {
  const bot = normalizeBot({ id: 'b', skills: ['pdf'], fastModeSupport: 'offered' })
  const [fast, pdf] = slashCommands(bot, '', installed)
  assert.equal(returnRuns(fast!, '/'), true)
  assert.equal(returnRuns(pdf!, '/pd'), true)
  assert.equal(returnRuns(pdf!, '/pdf'), false)
})
