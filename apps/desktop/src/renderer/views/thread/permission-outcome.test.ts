import assert from 'node:assert/strict'
import { test } from 'node:test'
import { permissionOutcome } from './permission-outcome.ts'

const options = [
  { optionId: 'allow', name: 'Allow once', kind: 'allow_once' },
  { optionId: 'always', name: 'Always allow', kind: 'allow_always' },
  { optionId: 'deny', name: 'Deny', kind: 'reject_once' },
]

test('an answered card says which option was taken', () => {
  assert.equal(permissionOutcome({ data: { status: 'answered', selected: 'always', options } }), 'Always allowed')
  assert.equal(permissionOutcome({ data: { status: 'answered', selected: 'allow', options } }), 'Allowed once')
  assert.equal(permissionOutcome({ data: { status: 'answered', selected: 'deny', options } }), 'Denied')
})

test("an always the host couldn't keep shows the host's words", () => {
  const outcome = "Allowed once only: it can't be kept for always (sam has no membership of their own for house)"
  assert.equal(permissionOutcome({ data: { status: 'answered', selected: 'allow', options, outcome } }), outcome)
})

test('a This session card says whether its grant lives, was removed or ended', () => {
  const opts = [...options, { optionId: 'kith-auto-session', name: 'This session', kind: 'allow_session' }]
  const card = (sessionGrant?: object) => ({ data: { status: 'answered', selected: 'kith-auto-session', options: opts, outcome: 'Allowed this session', sessionGrant } as never })
  assert.equal(permissionOutcome(card()), 'Allowed this session')
  assert.equal(permissionOutcome(card({ id: 'g', state: 'live' })), 'Allowed this session')
  const at = new Date(2026, 10, 2, 19, 5).getTime()
  assert.equal(permissionOutcome(card({ id: 'g', state: 'removed', endedAt: at })), 'Allowed this session · removed 19:05')
  assert.equal(permissionOutcome(card({ id: 'g', state: 'ended', endReason: 'task' })), 'Allowed this session · ended with the task')
  assert.equal(permissionOutcome(card({ id: 'g', state: 'ended', endReason: 'expired' })), 'Allowed this session · ended after 24 h')
  assert.equal(permissionOutcome(card({ id: 'g', state: 'ended', endReason: 'settings' })), 'Allowed this session · ended, settings changed')
})

test('a card nobody answered is cancelled or expired', () => {
  assert.equal(permissionOutcome({ data: { status: 'cancelled', options } }), 'Cancelled')
  assert.equal(permissionOutcome({ data: { status: 'expired', options } }), 'Expired — the agent moved on')
})
