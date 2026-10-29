import assert from 'node:assert/strict'
import { test } from 'node:test'
import { alwaysAllowLabel, folderOf, islandOptionLabel } from './permission-labels.ts'

test('always allow names the folder', () => {
  assert.equal(alwaysAllowLabel('/Users/kai/hq/apps/ios'), 'Always allow in ios')
  assert.equal(alwaysAllowLabel('/Users/kai/hq/'), 'Always allow in hq')
})

test('always allow has no folder to name', () => {
  assert.equal(alwaysAllowLabel(undefined), 'Always allow')
  assert.equal(alwaysAllowLabel(''), 'Always allow')
  assert.equal(alwaysAllowLabel('/'), 'Always allow')
  assert.equal(folderOf(null), null)
})

test('the island uses Allow, Deny and Always', () => {
  assert.equal(islandOptionLabel('allow_once', 'Allow once'), 'Allow')
  assert.equal(islandOptionLabel('reject_once', 'Reject'), 'Deny')
  assert.equal(islandOptionLabel('allow_always', 'Always allow'), 'Always')
  assert.equal(islandOptionLabel('reject_always', 'Never'), 'Never')
})
