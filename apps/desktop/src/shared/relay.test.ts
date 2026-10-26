import assert from 'node:assert/strict'
import { test } from 'node:test'
import { accessProgress, cloudAccess, cloudRole, relayComputers } from './relay.ts'

test("a computer's role: absent is the owner (older cloud), a role this version doesn't know is the least", () => {
  assert.equal(cloudRole('owner'), 'owner')
  assert.equal(cloudRole('member'), 'member')
  assert.equal(cloudRole(undefined), 'owner')
  assert.equal(cloudRole(null), 'owner')
  // The same as iOS (ComputerRole decodes an unknown role as member): never owner-only screens by accident.
  assert.equal(cloudRole('admin'), 'member')
  assert.equal(cloudRole(''), 'member')
  assert.equal(cloudRole(1), 'member')
})

test("the cloud's access words, and one this version doesn't know is not access", () => {
  for (const w of ['granted', 'pending', 'none'] as const) assert.equal(cloudAccess(w), w)
  // The #328 word: the cloud never sends it, and it must never read as access.
  assert.equal(cloudAccess('approved'), null)
  assert.equal(cloudAccess(undefined), null)
})

test('GET /v1/computers rows map the same for desktop and web', () => {
  // Shaped like the cloud's computerView (serve/cloud/src/routes/common.ts).
  const body = {
    computers: [
      { role: 'member', computerId: 'cmp_a', name: 'Kai’s Mac', platform: 'macos', device: 'MacBookPro18,3', signKey: 'k1', boxKey: 'b1',
        version: '0.1.0', online: true, lastSeenAt: 1, claimedAt: 2, access: 'granted' },
      { computerId: 'cmp_b', signKey: 'k2', access: 'pending', role: 'someday' },
      { name: 'no id' },
      null,
    ],
  }
  assert.deepEqual(relayComputers(body), [
    { computerId: 'cmp_a', name: 'Kai’s Mac', role: 'member', platform: 'macos', signKey: 'k1', online: true, access: 'granted' },
    { computerId: 'cmp_b', name: 'Mac', role: 'member', platform: '', signKey: 'k2', online: false, access: 'pending' },
  ])
  assert.deepEqual(relayComputers({}), [])
  assert.deepEqual(relayComputers(null), [])
})

test('access request status: the five words, anything else is over', () => {
  for (const w of ['pending', 'approved', 'denied', 'expired', 'cancelled'] as const) assert.equal(accessProgress(w), w)
  assert.equal(accessProgress('granted'), 'expired')
  assert.equal(accessProgress(undefined), 'expired')
})
