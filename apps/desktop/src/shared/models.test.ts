import assert from 'node:assert/strict'
import { test } from 'node:test'
import { draftOf, isChat, needsKithAgent, isWorkingIn, normalizeBot, type Entry } from './models.ts'

test('a bot works in every place its host lists at once, and only there', () => {
  const bot = normalizeBot({
    id: 'b', status: 'working', workingChat: 'b',
    workingLanes: [{ chat: 'b', thread: null }, { chat: 'b', thread: 'root' }, { chat: 'group' }],
  })
  assert.ok(isWorkingIn(bot, 'b', null))
  assert.ok(isWorkingIn(bot, 'b', 'root'))
  assert.ok(isWorkingIn(bot, 'group', undefined))
  assert.ok(!isWorkingIn(bot, 'b', 'other'))
  assert.ok(!isWorkingIn({ ...bot, workingLanes: [] }, 'b', null), 'an empty list means nowhere')
  assert.ok(isWorkingIn({ ...bot, workingLanes: null }, 'b', null), 'an older host names one place')
})

test('a bot keeps its Fast mode through the settings draft', () => {
  const bot = normalizeBot({ id: 'b', backend: 'codex', fastMode: true })
  assert.equal(bot.fastMode, true)
  assert.equal(draftOf(bot).fastMode, true)
})

test('a host without Fast mode reads as off', () => {
  assert.equal(normalizeBot({ id: 'b' }).fastMode, false)
})

test('the composer offers Fast mode until the agent says it has none, and while it is on', async () => {
  const { fastModeShown, fastModeState, fastModeDetail } = await import('./fast-mode.ts')
  assert.equal(fastModeShown(normalizeBot({ id: 'b' })), true)
  assert.equal(fastModeShown(normalizeBot({ id: 'b', fastModeSupport: 'unsupported' })), false)
  assert.equal(fastModeShown(normalizeBot({ id: 'b', fastModeSupport: 'offered' })), true)
  assert.equal(fastModeShown(normalizeBot({ id: 'b', fastMode: true, fastModeSupport: 'unsupported' })), true)
  assert.equal(fastModeShown(normalizeBot({ id: 'g', kind: 'group', fastModeSupport: 'offered' })), false)
  const refused = normalizeBot({ id: 'b', fastMode: true, fastModeSupport: 'refused', fastModeIssue: 'requires extra usage' })
  assert.equal(fastModeState(refused), 'refused')
  assert.match(fastModeDetail(refused), /requires extra usage/)
})


test('memory lines and summaries stay out of the chat', () => {
  const notice = (data: Entry['data']) => ({ id: 'n', botId: 'b', threadId: null, kind: 'notice', rev: 1, turn: 1, createdAt: 0, data }) as Entry
  assert.equal(isChat(notice({ text: 'Remembered: likes tea', memory: { status: 'done' } })), false)
  assert.equal(isChat(notice({ text: 'Updated people.md', memoryFile: { status: 'done' } })), false)
  assert.equal(isChat(notice({ text: 'Earlier context was summarized to make room.', compaction: { byHost: true } })), false)
  assert.equal(isChat(notice({ text: 'Stopped.' })), true)
})

test('a kept handoff leaves no line; a fresh conversation does', () => {
  const notice = (status: string) => ({ id: 'n', botId: 'b', threadId: null, kind: 'notice', rev: 1, turn: 1, createdAt: 0, data: { text: '…', handoff: { status } } }) as Entry
  assert.equal(isChat(notice('kept')), false)
  assert.equal(isChat(notice('sent')), true)
  assert.equal(isChat(notice('draft')), true)
})

test('only a native row is flagged: a room never is, and an older host sends no flag', () => {
  assert.equal(needsKithAgent(normalizeBot({ id: 'n', needsKithAgent: true })), true)
  assert.equal(needsKithAgent(normalizeBot({ id: 'g', kind: 'group', needsKithAgent: true })), false)
  assert.equal(needsKithAgent(normalizeBot({ id: 'k', hqAgent: 'house' })), false)
  assert.equal(needsKithAgent(normalizeBot({ id: 'old' })), false)
})
