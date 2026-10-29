import assert from 'node:assert/strict'
import { test } from 'node:test'
import { permissionChoices } from './permission-options.ts'

// The host's card for Sam's run reaching a host that is part of a job not on for Sam (cards/edge.rs, cards/mod.rs).
const edgeOptions = [
  { optionId: 'allow', name: 'Just this once', kind: 'allow_once' },
  { optionId: 'kith-always', name: 'Always for Sam', kind: 'allow_always' },
  { optionId: 'kith-job', name: 'Turn on Make videos: fonts.example.org, money', kind: 'allow_always' },
  { optionId: 'deny', name: 'No', kind: 'reject_once' },
]

test("an edge card shows each of the host's options in its words", () => {
  const choices = permissionChoices({ options: edgeOptions, edge: { kind: 'domains', value: 'fonts.example.org', job: 'make-videos' } })
  assert.deepEqual(choices, [
    { id: 'allow', label: 'Just this once' },
    { id: 'kith-always', label: 'Always for Sam' },
    { id: 'kith-job', label: 'Turn on Make videos: fonts.example.org, money' },
    { id: 'deny', label: 'No' },
  ])
})

test('an edge card with one always is still listed by name', () => {
  const options = edgeOptions.filter((o) => o.optionId !== 'kith-job')
  assert.deepEqual(permissionChoices({ options, edge: { kind: 'domains', value: 'fonts.example.org' } })?.map((c) => c.label), ['Just this once', 'Always for Sam', 'No'])
})

test("an agent's own allow / always / deny card stays the plain card, and any other set goes by kind", () => {
  const options = [
    { optionId: 'a', name: 'Allow', kind: 'allow_once' },
    { optionId: 'b', name: 'Always Allow', kind: 'allow_always' },
    { optionId: 'c', name: 'Reject', kind: 'reject_once' },
  ]
  assert.equal(permissionChoices({ options }), null)
  const never = [...options, { optionId: 'd', name: 'Reject always', kind: 'reject_always' }]
  assert.deepEqual(permissionChoices({ options: never })?.map((c) => c.label), ['Allow once', 'Always allow', 'Deny', 'Never'])
})
