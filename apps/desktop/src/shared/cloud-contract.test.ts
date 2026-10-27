// The cloud contract (serve/contract/cloud/contract.json) as desktop and web read it: the generated file is the
// contract's, and the answers of today's, an older (compat/) and a newer (unknown/) cloud all read.

import assert from 'node:assert/strict'
import { createHash } from 'node:crypto'
import { readdirSync, readFileSync } from 'node:fs'
import { test } from 'node:test'
import { askForAccess, type RelayApi } from './relay-access.ts'
import { check, CONTRACT_SHA256, receive, type TypeName } from './cloud-contract.gen.ts'
import { relayComputers } from './relay.ts'

const dir = new URL('../../../../serve/contract/cloud/', import.meta.url)
const fixtures = (sub: '' | 'compat/' | 'unknown/') =>
  readdirSync(new URL(`fixtures/${sub}`, dir))
    .filter((f) => f.endsWith('.json'))
    .map((f) => ({ f, ...(JSON.parse(readFileSync(new URL(`fixtures/${sub}${f}`, dir), 'utf8')) as { type: TypeName; body: unknown }) }))

test('cloud-contract.gen.ts is made from contract.json as it is now (else: node serve/contract/gen.mjs)', () => {
  assert.equal(createHash('sha256').update(readFileSync(new URL('contract.json', dir))).digest('hex'), CONTRACT_SHA256)
})

test("today's, an older and a newer cloud's answers all read", () => {
  for (const sub of ['', 'compat/', 'unknown/'] as const) {
    const all = fixtures(sub)
    assert.ok(all.length > 0, sub)
    for (const { f, type, body } of all) assert.deepEqual(check(type, body, 'consumer'), [], `${sub}${f}`)
  }
})

test("a newer cloud's words read as the contract says: role member, access none, a request state 'unknown'", () => {
  const [list] = fixtures('unknown/').filter((x) => x.type === 'ComputerList')
  const [row] = receive('ComputerList', list.body).computers
  assert.deepEqual([row.role, row.access], ['member', 'none'])
  // Fields this version doesn't know are passed through, not refused.
  assert.ok('newField' in row)
  const [c] = relayComputers(list.body)
  assert.deepEqual([c.role, c.access], ['member', 'none'])
  const [status] = fixtures('unknown/').filter((x) => x.type === 'AccessStatus')
  assert.equal(receive('AccessStatus', status.body).status, 'unknown')
})

test("the cloud never sends 'unknown': the producer check refuses it", () => {
  const computer = { computerId: 'c', name: 'n', signKey: 'k' }
  assert.deepEqual(check('CloudComputer', { ...computer, role: 'unknown' }, 'consumer'), [])
  assert.notDeepEqual(check('CloudComputer', { ...computer, role: 'unknown' }, 'producer'), [])
  assert.notDeepEqual(check('AccessStatus', { status: 'unknown' }, 'producer'), [])
})

test("an older cloud's computers (no role, no access) are the owner's, with no access", () => {
  const [list] = fixtures('compat/').filter((x) => x.type === 'ComputerList')
  const rows = relayComputers(list.body)
  assert.ok(rows.length > 0)
  for (const c of rows) assert.equal(c.role, 'owner')
})

test("an access request in a state this version doesn't know is over, never approved", async () => {
  const api = {
    requestAccess: async () => ({ ok: true, value: { requestId: 'r', code: '123456', expiresAt: Date.now() + 60_000, computerId: 'c' } }),
    accessStatus: async () => ({ ok: true, value: 'unknown' }),
  } as unknown as RelayApi
  assert.equal(await askForAccess(api, { computerId: 'c', signKey: 'k' }, { sleep: async () => {} }), 'expired')
})
