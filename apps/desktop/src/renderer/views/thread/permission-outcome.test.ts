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

test('a card nobody answered is cancelled or expired', () => {
  assert.equal(permissionOutcome({ data: { status: 'cancelled', options } }), 'Cancelled')
  assert.equal(permissionOutcome({ data: { status: 'expired', options } }), 'Expired — the agent moved on')
})
