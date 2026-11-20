import assert from 'node:assert/strict'
import { test } from 'node:test'
import { normalizeBot } from '../../../shared/models.ts'
import { returnRuns, slashCommands, slashQuery } from './slash-commands.ts'

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

test("the list holds only the bot's own skills, by name or title", () => {
  const bot = normalizeBot({ id: 'b', skills: ['pdf', 'code-review'] })
  assert.deepEqual(slashCommands(bot, '', installed).map((c) => c.name), ['pdf', 'code-review'])
  assert.deepEqual(slashCommands(bot, 'review', installed).map((c) => c.name), ['code-review'])
  assert.deepEqual(slashCommands(bot, 'fast', installed), [], 'Fast mode is gone')
  assert.deepEqual(slashCommands(normalizeBot({ id: 'b', skills: ['pdf'] }), 'xl', installed), [])
  assert.deepEqual(slashCommands(normalizeBot({ id: 'b' }), '', []), [])
  assert.deepEqual(slashCommands(normalizeBot({ id: 'g', kind: 'group' }), '', installed), [])
})

test('Return fills in the highlighted skill, but sends one already typed out', () => {
  const bot = normalizeBot({ id: 'b', skills: ['pdf'] })
  const [pdf] = slashCommands(bot, '', installed)
  assert.equal(returnRuns(pdf!, '/'), true)
  assert.equal(returnRuns(pdf!, '/pd'), true)
  assert.equal(returnRuns(pdf!, '/pdf'), false)
})
