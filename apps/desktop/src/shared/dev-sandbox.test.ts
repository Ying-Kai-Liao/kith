import assert from 'node:assert/strict'
import { test } from 'node:test'
import { sandboxLabel, sandboxName, sandboxPorts } from './dev-sandbox.ts'

test('sandbox ports skip the ones other sandboxes hold and never reach Kith', () => {
  const ports = sandboxPorts([19300, 19302])
  assert.deepEqual(ports.slice(0, 2), [19301, 19303])
  assert.equal(ports.length, 98)
  assert.ok(ports.every((port) => port >= 19300 && port <= 19399))
})

test('a sandbox is named after its worktree folder', () => {
  assert.equal(sandboxName('feat details/panel'), 'feat-details-panel')
  assert.equal(sandboxLabel('Kith', '/Users/me/.kith-dev-feat-x'), 'Kith (feat-x)')
  assert.equal(sandboxLabel('Kith', 'C:\\Users\\me\\box\\'), 'Kith (box)')
})
