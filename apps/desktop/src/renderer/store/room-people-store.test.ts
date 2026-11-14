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

const ann = { personId: 'ann', name: 'Ann', agents: { planner: 'guest' }, everyoneAgents: {}, createdAt: 1, projects: [], joined: true, openInvites: 0, devices: [], autoBots: false, revoked: false }

function rig(opts: { failAdd?: string } = {}) {
  const calls: string[] = []
  const client = {
    roomPeople: async (room: string) => ({ enabled: true, gen: 1, people: [{ person: 'ann', joinedGen: 1, leftGen: null }], room }),
    addRoomPerson: async (room: string, person: string) => {
      calls.push(`add ${room} ${person}`)
      await new Promise((resolve) => setTimeout(resolve, 5))
      if (opts.failAdd) throw new Error(opts.failAdd)
      return { gen: 2, changed: true }
    },
    removeRoomPerson: async (room: string, person: string) => void calls.push(`remove ${room} ${person}`),
    setPersonAccess: async (id: string, agents: Record<string, string>) => void calls.push(`access ${id} ${JSON.stringify(agents)}`),
    people: async () => [ann],
    invites: async () => [],
  }
  const store = { connection: { kind: 'online' }, isOwner: true, client, ready: async () => client } as unknown as BotStore
  return { store, calls }
}

test('a double tap on Add sends one call', async () => {
  const { RoomPeopleModel } = await import('./room-people-store.ts')
  const { store, calls } = rig()
  const model = new RoomPeopleModel(store, 'room-1')
  await Promise.all([model.add('ann'), model.add('ann')])
  assert.deepEqual(calls, ['add room-1 ann'])
  assert.equal(model.enabled, true)
  assert.equal(model.stays.length, 1)
})

test('give access and add: grant merged with what the person holds, then add', async () => {
  const { RoomPeopleModel } = await import('./room-people-store.ts')
  const { store, calls } = rig()
  const model = new RoomPeopleModel(store, 'room-1')
  assert.equal(await model.grantAndAdd(ann as never, { coder: 'guest' }, ['coder']), true)
  assert.deepEqual(calls, ['access ann {"planner":"guest","coder":"guest"}', 'add room-1 ann'])
})

test('an add that fails after the grant says both outcomes and keeps the grant', async () => {
  const { RoomPeopleModel } = await import('./room-people-store.ts')
  const { store, calls } = rig({ failAdd: 'Remove Grok from Studio first.' })
  const model = new RoomPeopleModel(store, 'room-1')
  assert.equal(await model.grantAndAdd(ann as never, { coder: 'member' }, ['coder']), false)
  assert.equal(model.error, "Ann now has coder, but couldn't join: Remove Grok from Studio first.")
  assert.equal(calls.filter((c) => c.startsWith('access')).length, 1)
})

test('a host before rooms with people leaves the section off', async () => {
  const { RoomPeopleModel } = await import('./room-people-store.ts')
  const { store } = rig()
  const client = { roomPeople: async () => { throw Object.assign(new Error('unknown method roomPeople'), { status: 404 }) } }
  Object.assign(store, { client })
  const model = new RoomPeopleModel(store, 'room-1')
  await model.load()
  assert.notEqual(model.enabled, true)
})
