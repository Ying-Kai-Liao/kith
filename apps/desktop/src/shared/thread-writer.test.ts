import assert from 'node:assert/strict'
import { test } from 'node:test'
import { normalizeBot, type Entry, type ThreadSummary } from './models.ts'
import { taskStatus } from './tasks.ts'
import {
  isHeldByTerminal, isHeldElsewhere, isTerminalOrigin, resumeCommand, runLabel, threadOrigin, threadWriterCode, threadWriterMessage, writerChange,
} from './thread-writer.ts'

const base: ThreadSummary = { count: 2, lastAt: 5, authors: ['b'] }
const entry = (id: string, kind: Entry['kind'], data: Entry['data'], threadId: string | null = null): Entry =>
  ({ id, seq: 0, botId: 'b', threadId, rev: 1, kind, turn: 1, data, createdAt: 0, updatedAt: 0 })

test('an app thread has no writer fields and is not held', () => {
  assert.equal(threadOrigin(base), 'app')
  assert.equal(isTerminalOrigin(base), false)
  assert.equal(isHeldElsewhere(base), false)
  assert.equal(isHeldElsewhere(undefined), false)
  assert.equal(isHeldElsewhere({ liveWriter: null }), false)
  assert.equal(isHeldElsewhere({ liveWriter: 'app' }), false)
})

test('a terminal thread is held by the terminal; an app thread opened in a terminal is held but not terminal-origin', () => {
  const s: ThreadSummary = { ...base, origin: 'terminal', originLabel: 'from terminal', liveWriter: 'terminal', liveBusy: true, terminal: { sessionId: 's', cwd: '/w' } }
  assert.equal(isTerminalOrigin(s), true)
  assert.equal(isHeldElsewhere(s), true)
  assert.equal(isHeldByTerminal(s), true)
  const opened: ThreadSummary = { ...base, liveWriter: 'terminal' }
  assert.equal(isTerminalOrigin(opened), false)
  assert.equal(isHeldElsewhere(opened), true)
})

test('unknown origins and writers are valid; an unknown writer holds the thread', () => {
  assert.deepEqual(threadOrigin({ origin: 'matrix' }), { other: 'matrix' })
  assert.equal(threadOrigin({ origin: 'discord' }), 'discord')
  assert.equal(isHeldElsewhere({ liveWriter: 'bot' }), true)
  assert.equal(isHeldByTerminal({ liveWriter: 'bot' }), false)
})

test('a summary survives a JSON round trip (the bot cache)', () => {
  const s: ThreadSummary = { ...base, origin: 'bot', originBot: { provider: 'p', owner: 'o', name: 'n' }, liveWriter: 'bot' }
  assert.deepEqual(JSON.parse(JSON.stringify(s)), s)
})

test('writer-change notices read their word, unknown ones too', () => {
  assert.equal(writerChange({ writerChange: 'tookOver' }), 'tookOver')
  assert.deepEqual(writerChange({ writerChange: 'later' }), { other: 'later' })
  assert.equal(writerChange({}), null)
})

test('the resume command names the root entry', () => {
  assert.equal(resumeCommand('house', { id: 'e_1' }), 'kith house --resume e_1')
})

test('host 409s about the writer become codes; other errors do not', () => {
  assert.equal(threadWriterCode({ status: 409, message: 'liveInTerminal: Take it over.' }), 'liveInTerminal')
  assert.equal(threadWriterCode({ status: 409, message: 'terminalBusy: Wait.' }), 'terminalBusy')
  assert.equal(threadWriterCode({ status: 409, message: 'heldByOtherWriter: x' }), 'heldByOtherWriter')
  assert.equal(threadWriterCode({ status: 409, message: 'paused' }), null)
  assert.equal(threadWriterCode({ status: 400, message: 'liveInTerminal: x' }), null)
  assert.equal(threadWriterCode(null), null)
  assert.equal(threadWriterMessage('terminalBusy: Wait.'), 'Wait.')
})

test('a prompt in a held thread does not need you', () => {
  const bot = normalizeBot({ id: 'b', workingLanes: [{ chat: 'b', thread: 't' }] })
  const prompt = entry('p', 'permission', { status: 'pending' }, 't')
  assert.equal(taskStatus(bot, entry('t', 'user', { thread: base }), [prompt]), 'needsYou')
  assert.equal(taskStatus(bot, entry('t', 'user', { thread: { ...base, liveWriter: 'terminal' } }), [prompt]), 'working')
})

test('a mirrored one-shot says who started it; a real terminal and other origins say nothing', () => {
  assert.equal(runLabel({ origin: 'terminal', originLabel: ' asked by architect ' }), 'asked by architect')
  assert.equal(runLabel({ origin: 'terminal' }), undefined)
  assert.equal(runLabel({ origin: 'terminal', originLabel: '  ' }), undefined)
  assert.equal(runLabel({ origin: 'discord', originLabel: "Sam's server" }), undefined)
  assert.equal(runLabel(undefined), undefined)
})
