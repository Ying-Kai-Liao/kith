import assert from 'node:assert/strict'
import { test } from 'node:test'
import { CANT_STOP, confirmMessage, failureOf, STILL_ENDING, heldIcon, heldRowOf, IDLE, isHeldPermission, OWN_TURN, sinceText, stoppedDivider, taskLabel } from './held-run.ts'
import type { Entry, HeldRun } from './models.ts'

const held: HeldRun = { session: 's1', kind: 'terminal', pid: 4812, since: new Date(2026, 9, 24, 9, 40).toISOString() }
const now = new Date(2026, 9, 24, 12, 0)

const entry = (data: Record<string, unknown>): Entry => ({ id: 'e1', seq: 1, botId: 'b', rev: 1, kind: 'permission', turn: 1, data, createdAt: 1, updatedAt: 1 }) as unknown as Entry

test('entries without held, or with held null, are unchanged and show no row', () => {
  assert.equal(isHeldPermission(entry({ status: 'answered' })), false)
  assert.equal(isHeldPermission(entry({ status: 'answered', held: null })), false)
  assert.equal(isHeldPermission(entry({ status: 'answered', held })), true)
})

test('a pending card never counts as held, and a held row adds no needs-you', () => {
  const pending = entry({ status: 'pending', held })
  assert.equal(isHeldPermission(pending), false)
  assert.equal(taskLabel('Done', [pending]), 'Done')
  assert.equal(taskLabel('Done', [entry({ status: 'answered', held })]), 'Waiting for an old run')
})

test('only the owner sees Stop; members and bot guests read who can stop it', () => {
  const owner = heldRowOf({ held, owner: true, now })
  const member = heldRowOf({ held, owner: false, ownerName: 'Kai', now })
  assert.equal(owner.button, 'Stop')
  assert.equal(member.button, null)
  assert.equal(owner.sentence, 'Since 9:40. It goes on when that ends.')
  assert.equal(member.sentence, 'Since 9:40. It goes on when that ends, or when Kai stops it.')
  assert.ok(heldRowOf({ held, owner: false, now }).sentence.endsWith('or when the owner stops it.'))
})

test('the copy and icon follow the kind; the pid is only in the tooltip', () => {
  const title = (kind: string) => heldRowOf({ held: { ...held, kind }, owner: true, now }).title
  assert.equal(title('terminal'), 'An old terminal run still has this session')
  assert.equal(title('oneshot'), 'An old background run still has this session')
  assert.equal(title('bot'), 'An old chat run still has this session')
  assert.equal(title('app'), 'An old app turn still has this session')
  assert.equal(heldIcon('oneshot'), 'gearshape.2')
  assert.equal(heldIcon('app'), 'macwindow')
  const row = heldRowOf({ held, owner: true, now })
  assert.equal(row.tooltip, 'terminal · pid 4812 · since 9:40')
  assert.ok(!row.sentence.includes('4812') && !row.title.includes('4812'))
})

test('states: stopping, failed with Try again, hidden after a refusal or the host own turn', () => {
  const stopping = heldRowOf({ held, owner: true, phase: { kind: 'stopping' }, now })
  assert.equal(stopping.sentence, 'Stopping it…')
  assert.equal(stopping.stopping, true)
  const failed = heldRowOf({ held, owner: true, phase: failureOf(new Error('boom'), 'Mac mini'), now })
  assert.equal(failed.button, 'Try again')
  assert.equal(failed.failed, true)
  assert.equal(failed.sentence, "Couldn't stop it. Try again, or close it on Mac mini.")
  assert.equal(heldRowOf({ held, owner: true, phase: failureOf({ status: 403 }, 'Mac mini'), now }).button, null)
  assert.equal(heldRowOf({ held, owner: true, phase: OWN_TURN, now }).button, null)
  assert.equal(heldRowOf({ held, owner: true, phase: IDLE, now }).button, 'Stop')
  // A Stop the host answered with stillEnding / cantStop is told: the first can be pressed again, the second can't.
  const ending = heldRowOf({ held, owner: true, phase: STILL_ENDING, now })
  assert.equal(ending.failed, true)
  assert.equal(ending.button, 'Try again')
  assert.match(ending.sentence, /Still ending/)
  const cant = heldRowOf({ held, owner: true, phase: CANT_STOP, now })
  assert.equal(cant.button, null)
  assert.match(cant.sentence, /can't stop that run/)
})

test('the confirm names the Mac and the pid', () => {
  const text = confirmMessage({ held, mac: 'Mac mini', agent: 'coder', now })
  assert.equal(text, "A terminal session on Mac mini has had this session since 9:40. Kith ends it (pid 4812) so coder can go on with what you allowed. Anything it's in the middle of stops too.")
})

test('since reads as a clock, then yesterday, then a date; the divider names the run', () => {
  assert.equal(sinceText(new Date(2026, 9, 23, 21, 10).toISOString(), now), 'yesterday 21:10')
  assert.equal(sinceText(new Date(2026, 8, 1, 8, 5).toISOString(), now), 'Sep 1')
  assert.equal(sinceText('nonsense', now), '')
  assert.equal(stoppedDivider('terminal', new Date(2026, 9, 24, 10, 2)), 'Stopped the old terminal run · 10:02')
})
