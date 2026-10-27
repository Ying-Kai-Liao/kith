import assert from 'node:assert/strict'
import { test } from 'node:test'
import { boardTop, boardWaiting, buildBoard, needsYouRows, pendingQuestions, projectDetail, type BoardComputer } from './board.ts'
import { normalizeBot, type Bot, type Entry } from './models.ts'

const NOW = Date.parse('2026-10-24T12:00:00')
const min = (n: number) => NOW - n * 60_000

const entry = (id: string, botId: string, kind: Entry['kind'], data: Entry['data'], at: number, threadId: string | null = null): Entry =>
  ({ id, seq: 0, botId, threadId, rev: 1, kind, turn: 1, data, createdAt: at, updatedAt: at })
const task = (id: string, botId: string, text: string, at: number) => entry(id, botId, 'user', { text, task: true }, at)
const ask = (id: string, botId: string, at: number, threadId: string | null = null, extra: Entry['data'] = {}) =>
  entry(id, botId, 'permission', { status: 'pending', toolKind: 'execute', title: 'npm test', ...extra }, at, threadId)

const computer = (bots: Bot[], entries: Entry[], extra: Partial<BoardComputer> = {}): BoardComputer => ({
  id: 'mac',
  hostName: 'Mac mini',
  bots,
  entries: new Map(bots.map((b) => [b.id, entries.filter((e) => e.botId === b.id)])),
  ...extra,
})

test('questions waiting on you group by thread and count on the tab', () => {
  const designer = normalizeBot({ id: 'designer', name: 'designer', workingLanes: [{ chat: 'designer', thread: 't1' }] })
  const board = buildBoard(
    [computer([designer], [task('t1', 'designer', 'App direction', min(30)), ask('p1', 'designer', min(20), 't1'), ask('p2', 'designer', min(10), 't1')])],
    NOW,
  )
  assert.equal(boardWaiting(board), 2)
  assert.deepEqual(needsYouRows(board.needsYou).map((r) => [r.title, r.count, r.question]), [['App direction', 2, 'Wants to run a command']])
  assert.equal(board.working.length, 0, 'a task waiting on you is in Needs you, not Working')
})

test("a guest card in the owner's own chat is not this account's question", () => {
  const bot = normalizeBot({ id: 'b', name: 'b' })
  const guest = ask('g', 'b', min(5), null, { approver: 'owner' })
  assert.equal(pendingQuestions([guest, ask('m', 'b', min(4))]), 1)
  assert.equal(buildBoard([computer([bot], [guest])], NOW).needsYou.length, 0)
})

test('guest requests count but have no row to open', () => {
  const board = buildBoard(
    [computer([], [], { guestPrompts: [{ entryId: 'e', botId: 'x', personName: 'Mia', agent: 'coder', title: 'rm -rf build', options: [], createdAt: min(1), entry: ask('e', 'x', min(1)) }] })],
    NOW,
  )
  assert.equal(boardWaiting(board), 1)
  assert.deepEqual(needsYouRows(board.needsYou), [])
})

test('working tasks, finished-today tasks and a bot at work in its main chat', () => {
  const coder = normalizeBot({ id: 'coder', name: 'coder', status: 'working', activity: 'Editing a.ts', workingLanes: [{ chat: 'coder', thread: 'w' }] })
  const house = normalizeBot({ id: 'house', name: 'house', status: 'working', activity: 'Shopping list', workingLanes: [{ chat: 'house', thread: null }], startedAt: min(1) })
  const image = normalizeBot({ id: 'image', name: 'image', workingLanes: [] })
  const old = normalizeBot({ id: 'old', name: 'old', workingLanes: [] })
  const board = buildBoard(
    [
      computer(
        [coder, house, image, old],
        [
          task('w', 'coder', 'Sleep notice fires twice', min(5)),
          task('d', 'image', '3 OG cards', min(90)),
          entry('r', 'image', 'agent', { final: true, text: 'Done: three cards' }, min(80), 'd'),
          task('x', 'old', 'Yesterday', NOW - 2 * 86_400_000),
        ],
      ),
    ],
    NOW,
  )
  assert.deepEqual(board.working.map((w) => [w.title, w.detail]), [['house', 'Shopping list'], ['Sleep notice fires twice', 'Editing a.ts']])
  assert.deepEqual(board.doneToday.map((w) => [w.title, w.detail]), [['3 OG cards', 'Done: three cards']])
})

test('Where we left off keeps the newest handoff that is still fresh and not kept', () => {
  const builder = normalizeBot({ id: 'builder', name: 'builder' })
  const stale = normalizeBot({ id: 'stale', name: 'stale' })
  const board = buildBoard(
    [
      computer(
        [builder, stale],
        [
          entry('h1', 'builder', 'notice', { handoff: { status: 'draft', text: 'old note' } }, min(300)),
          entry('h2', 'builder', 'notice', { handoff: { status: 'sent', text: '  next:   run kith doctor ' } }, min(60)),
          entry('h3', 'stale', 'notice', { handoff: { status: 'sent', text: 'too old' } }, NOW - 4 * 86_400_000),
        ],
      ),
    ],
    NOW,
  )
  assert.deepEqual(board.leftOff.map((h) => [h.botName, h.note]), [['builder', 'next: run kith doctor']])
})

test('projects count threads and open decisions, busiest first', () => {
  const folder = (project: string) => ({ kind: 'project' as const, project, path: null, name: project, inherited: false })
  const a = normalizeBot({ id: 'a', name: 'a', folder: folder('kith'), lastAt: 1 })
  const b = normalizeBot({ id: 'b', name: 'b', folder: folder('kith'), lastAt: 5, status: 'working' })
  const c = normalizeBot({ id: 'c', name: 'c', folder: folder('peace'), lastAt: 9 })
  const board = buildBoard([computer([a, b, c], [task('1', 'a', 'x', min(1)), task('2', 'b', 'y', min(1)), ask('q', 'b', min(1))])], NOW)
  assert.deepEqual(board.projects.map((p) => [p.name, projectDetail(p), p.target.botId]), [['kith', '2 threads · 1 decision open', 'b'], ['peace', '1 agent', 'c']])
})

test('a computer that cannot be reached waits, first; the top item follows the section order', () => {
  const bot = normalizeBot({ id: 'b', name: 'b', status: 'working', workingLanes: [{ chat: 'b', thread: null }] })
  const board = buildBoard(
    [computer([bot], [entry('u', 'b', 'user', { text: 'hi', status: 'queued' }, min(1))], { trouble: 'unreachable' })],
    NOW,
  )
  assert.deepEqual(board.waits.map((w) => w.kind), ['computer', 'queued'])
  assert.deepEqual(boardTop(board), { computerId: 'mac', botId: 'b' })
  assert.equal(boardTop(buildBoard([], NOW)), null)
})

test('a session working in a terminal is on the Board though no host lane runs it', () => {
  const architect = normalizeBot({ id: 'a', name: 'architect', workingLanes: [] })
  // Started days ago and still going: the Board shows it for being busy, not for being recent.
  const live = entry('t', 'a', 'user', { text: 'Review the diff', task: true, thread: { count: 4, lastAt: NOW - 3 * 86_400_000, authors: [], liveWriter: 'terminal', liveBusy: true } }, NOW - 3 * 86_400_000)
  const idle = entry('i', 'a', 'user', { text: 'Idle one', task: true, thread: { count: 4, lastAt: min(5), authors: [], liveWriter: 'terminal', liveBusy: false } }, min(30))
  const board = buildBoard([computer([architect], [live, idle])], NOW)
  assert.deepEqual(board.working.map((w) => [w.title, w.detail]), [['Review the diff', 'Working in the terminal']])
  assert.deepEqual(board.doneToday.map((w) => w.title), ['Idle one'])
})
