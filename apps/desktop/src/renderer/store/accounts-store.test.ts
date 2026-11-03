import assert from 'node:assert/strict'
import { existsSync } from 'node:fs'
import { registerHooks } from 'node:module'
import { test } from 'node:test'

registerHooks({ resolve(specifier, context, nextResolve) {
  if (specifier.startsWith('@shared/')) return nextResolve(new URL(`../../shared/${specifier.slice(8)}.ts`, import.meta.url).href, context)
  if (specifier.startsWith('.') && context.parentURL && !/\.[a-z]+$/.test(specifier)) {
    const url = new URL(`${specifier}.ts`, context.parentURL)
    if (existsSync(url)) return nextResolve(url.href, context)
  }
  return nextResolve(specifier, context)
} })

const { parseAccounts } = await import('../../shared/accounts.ts')
const { AccountsStore } = await import('./accounts-store.ts')
type HostClient = import('../client/host-client').HostClient

const read = (policy: string) => parseAccounts({ policy, accounts: [{ id: 'codex', harness: 'codex', status: 'ready', ready: true }] })

test('a write replaces the read with its own answer, without asking again', async () => {
  let reads = 0
  const client = {
    accounts: async () => (reads++, read('fill')),
    accountPolicy: async (p: string) => read(p),
  } as unknown as HostClient
  const store = new AccountsStore(async () => client)
  await store.load()
  assert.equal(store.accounts.policy, 'fill')
  await store.setPolicy('spread')
  assert.equal(store.accounts.policy, 'spread')
  assert.equal(reads, 1)
  assert.equal(store.saving, false)
})

test('writes go one after the other and the last one stays; a failure shows inline and keeps the read', async () => {
  const order: string[] = []
  const client = {
    accounts: async () => read('fill'),
    accountPolicy: async (p: string) => {
      order.push(`start ${p}`)
      await new Promise((r) => setTimeout(r, p === 'spread' ? 20 : 0))
      order.push(`end ${p}`)
      return read(p)
    },
    accountClear: async () => { throw new Error('kith refused') },
  } as unknown as HostClient
  const store = new AccountsStore(async () => client)
  await store.load()
  const a = store.setPolicy('spread')
  const b = store.setPolicy('fill')
  assert.equal(store.saving, true)
  await Promise.all([a, b])
  assert.deepEqual(order, ['start spread', 'end spread', 'start fill', 'end fill'])
  assert.equal(store.accounts.policy, 'fill')
  assert.equal(await store.clear('codex'), false)
  assert.equal(store.error, 'kith refused')
  assert.equal(store.accounts.policy, 'fill')
  assert.equal(store.saving, false)
})

test('a host from before the account set is marked unsupported, not an error', async () => {
  const client = { accounts: async () => { throw Object.assign(new Error('unknown method'), { status: 404 }) } } as unknown as HostClient
  const store = new AccountsStore(async () => client)
  await store.load()
  assert.equal(store.unsupported, true)
  assert.equal(store.error, null)
})
