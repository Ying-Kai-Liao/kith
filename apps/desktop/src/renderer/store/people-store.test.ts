import assert from 'node:assert/strict'
import { existsSync } from 'node:fs'
import { registerHooks } from 'node:module'
import { test } from 'node:test'
import type { BotStore } from './bot-store'

registerHooks({ resolve(specifier, context, nextResolve) {
  if (specifier.startsWith('@shared/')) return nextResolve(new URL(`../../shared/${specifier.slice(8)}.ts`, import.meta.url).href, context)
  if (specifier.startsWith('.') && context.parentURL && !/\.[a-z]+$/.test(specifier)) {
    const url = new URL(`${specifier}.ts`, context.parentURL)
    if (existsSync(url)) return nextResolve(url.href, context)
  }
  return nextResolve(specifier, context)
} })

/** A host that records calls; `people` answers with whether the person is revoked right now. */
function rig(opts: { failUnrevoke?: string } = {}) {
  const calls: string[] = []
  let revoked = false
  const client = {
    revokePerson: async (id: string) => { calls.push(`revoke ${id}`); revoked = true },
    unrevokePerson: async (id: string) => {
      calls.push(`unrevoke ${id}`)
      await new Promise((resolve) => setTimeout(resolve, 5))
      if (opts.failUnrevoke) throw new Error(opts.failUnrevoke)
      revoked = false
    },
    people: async () => [{ personId: 'sam', name: 'Sam', agents: {}, projects: [], joined: true, openInvites: 0, devices: [], autoBots: false, revoked }],
    invites: async () => [],
  }
  const store = { connection: { kind: 'online' }, client, ready: async () => client } as unknown as BotStore
  return { store, calls }
}

test('removing a person offers Undo, and Undo restores them', async () => {
  const { PeopleModel } = await import('./people-store.ts')
  const { store, calls } = rig()
  const model = new PeopleModel(store)
  await model.load()
  await model.removePerson('sam')
  assert.deepEqual(model.removed, { personId: 'sam', name: 'Sam' })
  assert.equal(model.people[0]!.revoked, true)
  await model.restorePerson('sam')
  assert.equal(model.removed, null)
  assert.equal(model.people[0]!.revoked, false)
  assert.deepEqual(calls, ['revoke sam', 'unrevoke sam'])
})

test('a double click on Undo sends one call', async () => {
  const { PeopleModel } = await import('./people-store.ts')
  const { store, calls } = rig()
  const model = new PeopleModel(store)
  await model.removePerson('sam')
  await Promise.all([model.restorePerson('sam'), model.restorePerson('sam')])
  assert.equal(calls.filter((c) => c.startsWith('unrevoke')).length, 1)
})

test('a failed Undo shows the host reason and keeps the banner', async () => {
  const { PeopleModel } = await import('./people-store.ts')
  const { store } = rig({ failUnrevoke: 'Sam was removed for good.' })
  const model = new PeopleModel(store)
  await model.removePerson('sam')
  await model.restorePerson('sam')
  assert.equal(model.error, 'Sam was removed for good.')
  assert.equal(model.removed?.personId, 'sam')
})
