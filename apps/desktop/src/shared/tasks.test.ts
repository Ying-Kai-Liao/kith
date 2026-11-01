import assert from 'node:assert/strict'
import { test } from 'node:test'
import { normalizeBot, type Entry } from './models.ts'
import { chatAndTasks, continuingTask, currentTask, foldLine, sendInTask, takesOver, TASK_IDLE_MS, taskFold, taskHead, taskOf, taskOpen, taskStatus, taskTitle, workingThreadIn } from './tasks.ts'

const entry = (id: string, kind: Entry['kind'], data: Entry['data'], threadId: string | null = null): Entry =>
  ({ id, seq: 0, botId: 'b', threadId, rev: 1, kind, turn: 1, data, createdAt: 0, updatedAt: 0 })

test('the newest task is the current one; a plain message or a thread reply is no task', () => {
  const a = entry('a', 'user', { text: 'fix the build', task: true })
  const b = entry('b', 'user', { text: 'and the docs', task: true })
  assert.equal(currentTask([a, b, entry('n', 'notice', { text: 'Routine ran' })])?.id, 'b')
  assert.equal(currentTask([entry('u', 'user', { text: 'before tasks' })]), null)
  assert.equal(currentTask([entry('r', 'user', { task: true }, 'a')]), null)
})

test('a task is working, waiting on the user, stopped or done', () => {
  const root = entry('t', 'user', { text: 'deploy', task: true })
  const working = normalizeBot({ id: 'b', workingLanes: [{ chat: 'b', thread: 't' }] })
  const idle = normalizeBot({ id: 'b', workingLanes: [] })
  assert.equal(taskStatus(working, root, []), 'working')
  assert.equal(taskStatus(working, root, [entry('p', 'permission', { status: 'pending' }, 't')]), 'needsYou')
  assert.equal(taskStatus(idle, root, [entry('s', 'notice', { text: 'Stopped.' }, 't')]), 'stopped')
  assert.equal(taskStatus(idle, root, [entry('s', 'notice', { text: 'Stopped.' }, 't'), entry('r', 'agent', { final: true }, 't')]), 'done')
  assert.equal(taskStatus(idle, root, []), 'done')
  // A message parked at its agent's limit: no lane works, the card waits.
  assert.equal(taskStatus(idle, root, [entry('l', 'permission', { status: 'pending', toolKind: 'limit' }, 't')]), 'needsYou')
  assert.equal(taskStatus(idle, root, [entry('l', 'permission', { status: 'answered', toolKind: 'limit' }, 't')]), 'done')
})

test('a finished task takes the host\'s status, and an older host falls back to the thread', () => {
  const idle = normalizeBot({ id: 'b', workingLanes: [] })
  const thread = (status?: string) => entry('t', 'user', { text: 'deploy', task: true, thread: { count: 1, lastAt: 1, authors: [], status } })
  const failedNotice = [entry('f', 'notice', { text: 'The agent failed: boom' }, 't')]
  assert.equal(taskStatus(idle, thread('failed'), []), 'failed')
  assert.equal(taskStatus(idle, thread('stopped'), []), 'stopped')
  assert.equal(taskStatus(idle, thread('done'), failedNotice), 'done')
  assert.equal(taskStatus(idle, thread(), failedNotice), 'done', 'no status: today\'s derivation')
  assert.equal(taskStatus(idle, thread('working'), []), 'done', 'a stale working is not taken')
})

test('a voice call follows the chat and its tasks, not other threads', () => {
  const task = entry('t', 'user', { text: 'deploy', task: true })
  const plain = entry('p', 'agent', { text: 'hello' })
  const all = [plain, task, entry('a', 'agent', { final: true }, 't'), entry('x', 'agent', { final: true }, 'p')]
  assert.deepEqual(chatAndTasks(all).map((e) => e.id), ['p', 't', 'a'])
})

test('a card is titled by its first message on one line', () => {
  assert.equal(taskTitle(entry('t', 'user', { text: '  fix\nthe   build ', task: true })), 'fix the build')
  assert.equal(taskTitle(entry('t', 'user', { task: true })), 'Task')
})

test('a task a terminal is mid-turn in is working, though the host runs no lane for it', () => {
  const idle = normalizeBot({ id: 'b', workingLanes: [] })
  const thread = (liveWriter: string, liveBusy: boolean) => entry('t', 'user', { text: 'deploy', task: true, thread: { count: 1, lastAt: 1, authors: [], liveWriter, liveBusy } })
  assert.equal(taskStatus(idle, thread('terminal', true), []), 'working')
  assert.equal(taskStatus(idle, thread('terminal', false), []), 'done')
  assert.equal(taskStatus(idle, thread('app', true), []), 'done', "an app turn is the host's lane to report")
})

test('the flat chat finds where its turn runs, in a thread or in the chat itself', () => {
  assert.equal(workingThreadIn(normalizeBot({ id: 'b', workingLanes: [{ chat: 'b', thread: 't' }] }), 'b'), 't')
  assert.equal(workingThreadIn(normalizeBot({ id: 'b', workingLanes: [{ chat: 'b' }] }), 'b'), null)
  assert.equal(workingThreadIn(normalizeBot({ id: 'b', workingLanes: [{ chat: 'other' }] }), 'b'), undefined)
  assert.equal(workingThreadIn(normalizeBot({ id: 'b', workingLanes: [] }), 'b'), undefined)
})

test('a folded card shows the last reply and counts only what it hides', () => {
  const t = (id: string, kind: Entry['kind'], data: Entry['data']) => entry(id, kind, data, 'r')
  const thread = [
    t('a1', 'agent', { text: 'looking', final: true }),
    t('s1', 'tool', { title: 'Read x' }),
    t('s2', 'tool', { title: 'Edit x' }),
    t('u2', 'user', { text: 'also y' }),
    t('think', 'agent', { text: 'partial' }),
    t('a2', 'agent', { text: 'done', final: true }),
  ]
  const fold = taskFold(thread)
  assert.equal(fold.last?.id, 'a2')
  assert.deepEqual([fold.messages, fold.steps], [2, 2], 'a1 and u2, two tools; the shown reply and a partial are not counted')
  assert.equal(foldLine(fold.messages, fold.steps), '2 messages · 2 steps')
  assert.equal(foldLine(0, 1), '1 step')
  assert.equal(foldLine(0, 0), null, 'nothing hidden: no fold line')
  assert.deepEqual(taskFold([t('s', 'tool', {})], () => false), { last: null, messages: 0, steps: 0 }, 'a delivery is no step')
})

test('a task card is open while it works, folds when it stops, and the user\'s choice wins', () => {
  assert.equal(taskOpen('working'), true)
  assert.equal(taskOpen('needsYou'), false, 'folded, with its question on the card')
  assert.equal(taskOpen('done'), false)
  assert.equal(taskOpen('stopped'), false)
  assert.equal(taskOpen('done', true), true)
  assert.equal(taskOpen('working', false), false)
})

test('the head line says the status (none when done) and the origin only when it isn\'t the app', () => {
  assert.deepEqual(taskHead('done', undefined, 'Mac mini'), { status: null, origin: null, held: false })
  assert.deepEqual(taskHead('done', { count: 1, lastAt: 1, authors: [], origin: 'telegram' }, 'Mac mini'), { status: null, origin: 'Telegram', held: false })
  assert.deepEqual(taskHead('working', { count: 1, lastAt: 1, authors: [], origin: 'telegram', liveWriter: 'terminal' }, 'Mac mini'),
    { status: 'Working', origin: 'In terminal · Mac mini', held: true }, 'the writer chip takes the origin\'s place')
})

test('the box goes on with the newest task while it works or for ten idle minutes, never a terminal\'s', () => {
  const idle = normalizeBot({ id: 'b', workingLanes: [] })
  const at = (id: string, createdAt: number, data: Entry['data'] = {}) => ({ ...entry(id, 'user', { text: id, task: true, ...data }), createdAt })
  const now = 100 * TASK_IDLE_MS
  assert.equal(continuingTask(idle, [at('a', now - 60_000)], now)?.id, 'a')
  assert.equal(continuingTask(idle, [at('a', now - TASK_IDLE_MS)], now), null, 'ten quiet minutes: a new task')
  const working = normalizeBot({ id: 'b', workingLanes: [{ chat: 'b', thread: 'a' }] })
  assert.equal(continuingTask(working, [at('a', 0)], now)?.id, 'a')
  const terminal = at('t', now, { thread: { count: 1, lastAt: now, authors: [], origin: 'terminal' } })
  assert.equal(continuingTask(idle, [at('a', now - 60_000), terminal], now)?.id, 'a')
  assert.equal(taskOf(entry('r', 'agent', {}, 'a')), 'a')
  assert.equal(taskOf(at('a', 0)), 'a')
  assert.equal(taskOf(entry('n', 'user', { text: 'plain' })), null)
})

test('a send to a picked task an idle terminal holds goes only after the take-over; a failed one sends nothing', async () => {
  const held = { count: 1, lastAt: 1, authors: [], origin: 'terminal', liveWriter: 'terminal', liveBusy: false }
  const calls: string[] = []
  assert.equal(takesOver(held, true), true)
  const ok = await sendInTask(held, true, async () => { calls.push('take') }, () => calls.push('send'))
  assert.equal(ok, null)
  assert.deepEqual(calls, ['take', 'send'])

  calls.length = 0
  const failed = await sendInTask(held, true, async () => { calls.push('take'); throw new Error('terminalBusy') }, () => calls.push('send'))
  assert.equal(failed, 'terminalBusy', 'the caller keeps the text in the box and shows why')
  assert.deepEqual(calls, ['take'], 'never sent, so never a new task')

  calls.length = 0
  await sendInTask({ ...held, liveBusy: true }, true, async () => { calls.push('take') }, () => calls.push('send'))
  assert.deepEqual(calls, ['send'], 'a busy terminal is not taken from; the host queues the message')
  calls.length = 0
  await sendInTask(undefined, true, async () => { calls.push('take') }, () => calls.push('send'))
  assert.deepEqual(calls, ['send'])
})

test('a task the box went on with by itself is never taken from a terminal', async () => {
  const held = { count: 1, lastAt: 1, authors: [], origin: 'app', liveWriter: 'terminal', liveBusy: false }
  const calls: string[] = []
  assert.equal(takesOver(held, false), false)
  const sent = await sendInTask(held, false, async () => { calls.push('take') }, () => calls.push('send'))
  assert.equal(sent, null)
  assert.deepEqual(calls, ['send'], 'sent as the host routes it: queued behind the terminal')
})
