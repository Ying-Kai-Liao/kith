import assert from 'node:assert/strict'
import { test } from 'node:test'
import type { RichCard } from './rich-card.ts'
import { dayWord, liveKithJob, liveRoutine, quietEvery, scheduleFoot, scheduleView, shortPaths } from './schedule-card.ts'

// Thursday 29 Oct 2026, local time.
const at = (day: number, h: number, m: number) => new Date(2026, 9, day, h, m).getTime()
const now = at(29, 9, 20)

const job = (snapshot: Record<string, unknown> = {}): RichCard => ({
  type: 'schedule', v: 1, ref: { agent: 'personal', id: 'daily-news' },
  snapshot: {
    name: 'Daily news', op: 'added', at: at(29, 9, 12), person: null, cron: '43 7 * * *', every: 'Every day at 7:43',
    task: 'Daily news brief: read /Users/kai/hq/agents/personal/role/skills/learned/daily-news/SKILL.md and follow it.',
    paused: false, nextRun: new Date(at(30, 7, 43)).toISOString(), lastRun: null, lastStatus: null, ...snapshot,
  },
})
const view = (card: RichCard, extra: Partial<Parameters<typeof scheduleView>[1]> = {}) =>
  scheduleView(card, { postedAt: at(29, 9, 12), now, viewerIsPerson: true, personName: null, ...extra })

test('a new job reads from its snapshot: op and time, the schedule, next tomorrow, not run yet, as you', () => {
  const v = view(job())
  assert.equal(v.kind, 'job')
  assert.equal(v.op, 'Added · 9:12')
  assert.equal(v.every, 'Every day at 7:43')
  assert.equal(v.next, 'Tomorrow 7:43')
  assert.deepEqual(v.last, { text: 'Not run yet', running: false, failed: false, error: null })
  assert.equal(v.runsAs, 'you')
  assert.equal(v.task, 'Daily news brief: read …/learned/daily-news/SKILL.md and follow it.')
  assert.equal(v.fold, null)
})

test('live state wins over the snapshot: running since, a later next, paused', () => {
  const running = view(job(), { live: { paused: false, nextRun: at(30, 7, 43), last: { running: true, at: at(29, 7, 43) } } })
  assert.deepEqual(running.last, { text: 'Running since 7:43', running: true, failed: false, error: null })
  const paused = view(job(), { live: { paused: true, nextRun: null, last: { running: false, at: at(29, 7, 43) } } })
  assert.equal(paused.paused, true)
  assert.equal(paused.op, null)
  assert.equal(paused.next, 'Paused')
  assert.equal(paused.last.text, 'Today 7:43')
})

test('a job someone else runs as names them; their own reads "you"', () => {
  const card = job({ person: 'mia' })
  assert.equal(view(card, { viewerIsPerson: false, personName: 'Mia' }).runsAs, 'Mia')
  assert.equal(view(card, { viewerIsPerson: true, personName: 'Mia' }).runsAs, 'you')
})

test('a card the chat changed or removed is a line; a removed job reads removed', () => {
  assert.equal(view(job({ folded: 'changed' })).fold?.text, 'Daily news · changed below')
  assert.equal(view(job({ folded: 'removed' })).fold?.text, 'Daily news · removed below')
  assert.equal(view(job({ op: 'removed' })).fold?.text, 'Daily news · removed by you')
  const gone = view(job(), { live: null })
  assert.equal(gone.fold?.text, 'Daily news · removed by you')
  assert.equal(gone.fold?.opens, false)
})

test('after its day the card is a live pill that opens in place', () => {
  const live = { paused: false, nextRun: at(29, 9, 43), last: null }
  const old = view(job(), { postedAt: at(27, 9, 12), live })
  assert.deepEqual(old.fold, { text: 'Daily news · every day 7:43 · next 9:43', trailing: 'Tue', opens: true, done: false })
  assert.equal(view(job(), { postedAt: at(27, 9, 12), live, opened: true }).fold, null)
  assert.equal(view(job(), { postedAt: at(27, 9, 12), live: { ...live, paused: true } }).fold?.text, 'Daily news · every day 7:43 · paused')
})

test('a routine runs as its bot and shows a failed run with its error', () => {
  const card: RichCard = { type: 'schedule', v: 1, ref: { bot: 'b1', routine: 'r1' }, snapshot: { name: 'Issue watch', op: 'created', every: 'Every 30 minutes', task: 'Check issues', paused: false } }
  const live = liveRoutine({
    routines: [{ id: 'r1', enabled: true, nextRunAt: at(29, 11, 32), lastError: 'Timed out after 10 min' }],
    runs: [{ routineId: 'r1', status: 'failed', createdAt: at(29, 8, 52), finishedAt: at(29, 9, 2) }, { routineId: 'r1', status: 'done', createdAt: at(29, 8, 22) }],
  }, 'r1')
  const v = view(card, { live })
  assert.equal(v.kind, 'routine')
  assert.equal(v.runsAs, null)
  assert.equal(v.op, 'Created · 9:12')
  assert.deepEqual(v.last, { text: 'Today 9:02', running: false, failed: true, error: 'Timed out after 10 min' })
  assert.equal(v.next, 'Today 11:32')
  assert.equal(liveRoutine({ routines: [], runs: [] }, 'r1'), null)
})

test('a Kith job read live from kithInfo schedules, or gone', () => {
  const schedules = [{ id: 'daily-news', cron: '43 7 * * *', task: '', paused: false, lastRun: at(29, 7, 43), lastStatus: 'running', nextRun: at(30, 7, 43) }]
  assert.deepEqual(liveKithJob(schedules, 'daily-news'), { paused: false, nextRun: at(30, 7, 43), last: { running: true, at: at(29, 7, 43) } })
  assert.equal(liveKithJob(schedules, 'other'), null)
})

test('a snapshot that is missing pieces still draws', () => {
  const v = view({ type: 'schedule', v: 1 })
  assert.equal(v.name, 'Schedule')
  assert.equal(v.next, '—')
  assert.equal(v.last.text, 'Not run yet')
})

test('the foot: the owner gets Pause / Run now, Resume in ink when paused, and Running… until the run ends', () => {
  const foot = (card: RichCard, extra: Partial<Parameters<typeof scheduleFoot>[1]> = {}) =>
    scheduleFoot(view(card), { owner: true, ownerName: null, confirming: false, busy: null, ...extra })
  assert.deepEqual(foot(job()), {
    kind: 'buttons', toggle: { op: 'pause', label: 'Pause', ink: false }, run: { label: 'Run now', disabled: false }, busy: false,
  })
  const paused = foot(job({ paused: true }))
  assert.equal(paused.kind === 'buttons' && paused.toggle.label, 'Resume')
  assert.equal(paused.kind === 'buttons' && paused.toggle.ink, true)
  const pressed = foot(job(), { busy: 'run' })
  assert.deepEqual(pressed.kind === 'buttons' && pressed.run, { label: 'Running…', disabled: true })
  const running = foot(job({ lastRun: at(29, 9, 15), lastStatus: 'running' }))
  assert.deepEqual(running.kind === 'buttons' && running.run, { label: 'Running…', disabled: true })
  const pausing = foot(job(), { busy: 'pause' })
  assert.equal(pausing.kind === 'buttons' && pausing.run.disabled, true, 'one call at a time')
})

test('the foot: trash asks inside the card, and others read who can pause or run it', () => {
  const card = job()
  assert.deepEqual(scheduleFoot(view(card), { owner: true, ownerName: null, confirming: true, busy: null }), {
    kind: 'confirm', question: "Remove Daily news? It won't run again, and its task goes with it.", removing: false,
  })
  const removing = scheduleFoot(view(card), { owner: true, ownerName: null, confirming: true, busy: 'remove' })
  assert.equal(removing.kind === 'confirm' && removing.removing, true)
  assert.deepEqual(scheduleFoot(view(card), { owner: false, ownerName: 'Kai', confirming: false, busy: null }), { kind: 'note', text: 'Kai can pause or run it.' })
  assert.deepEqual(scheduleFoot(view(card), { owner: false, ownerName: null, confirming: true, busy: null }), { kind: 'note', text: 'The owner can pause or run it.' })
})

test('day words, quiet schedule words and short paths', () => {
  assert.equal(dayWord(at(30, 1, 0), now), 'Tomorrow')
  assert.equal(dayWord(at(31, 1, 0), now), 'Sat')
  assert.equal(dayWord(at(28, 1, 0), now), 'Yesterday')
  assert.equal(dayWord(at(5, 1, 0), now), '5 Oct')
  assert.equal(quietEvery('Weekdays at 7:30'), 'weekdays 7:30')
  assert.equal(quietEvery('Every 30 minutes'), 'every 30 minutes')
  assert.equal(shortPaths('read ~/a/b/c/d.md now'), 'read …/b/c/d.md now')
  assert.equal(shortPaths('read /b/c/d.md'), 'read /b/c/d.md')
})
