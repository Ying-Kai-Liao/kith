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

/** A store with just what an invited person's room code touches (the real constructor wants a browser). */
async function rig(rooms: { id: string; name: string; agents: string[]; stay: number }[]) {
  const { BotStore } = await import('./bot-store.ts')
  const sent: unknown[][] = []
  const client = {
    myRooms: async () => rooms,
    history: async () => { throw Object.assign(new Error('You are not in this room'), { status: 403 }) },
    send: async (...args: unknown[]) => { sent.push(args); throw new Error('stop') },
  }
  const store = Object.create(BotStore.prototype) as InstanceType<typeof BotStore> & Record<string, unknown>
  Object.assign(store, {
    client, retired: false, bots: new Map(), entries: new Map(), myRooms: new Map(), closedRooms: new Set(), closedRoomNames: new Map(),
    hello: { grantedAgents: [{ agent: 'planner' }] }, isLoopback: false, fixedRoute: 'relay', outgoingFiles: new Map(), outgoingPlaces: new Map(),
    outgoingBudgets: new Map(), outgoingEngines: new Map(), roleHint: { cloudRole: 'member', hasToken: true },
    scheduleSave() {}, changed() {}, upsert() {}, markLocal() {}, onRosterChanged: null,
  })
  return { store, sent }
}

test("a person's rooms show as groups; one gone from a refresh closes it: cache dropped, name kept for the closed state", async () => {
  const rooms = [{ id: 'room-1', name: 'Studio', agents: ['planner'], stay: 7 }]
  const { store } = await rig(rooms)
  await store.refreshMyRooms()
  assert.equal(store.bots.get('room-1')?.kind, 'group')
  assert.deepEqual(store.roster.map((b) => b.id), ['room-1'])
  store.entries.set('room-1', [{ id: 'e', seq: 1 } as never])
  rooms.length = 0
  await store.refreshMyRooms()
  assert.equal(store.closedRooms.has('room-1'), true)
  assert.equal(store.closedRoomNames.get('room-1'), 'Studio')
  assert.equal(store.entries.has('room-1'), false)
  assert.equal(store.bots.has('room-1'), false)
})

test('a refused room call closes the view and drops the cache', async () => {
  const { store } = await rig([{ id: 'room-1', name: 'Studio', agents: [], stay: 7 }])
  await store.refreshMyRooms()
  store.entries.set('room-1', [{ id: 'e', seq: 1 } as never])
  await store.loadRoom('room-1')
  assert.equal(store.closedRooms.has('room-1'), true)
  assert.equal(store.entries.has('room-1'), false)
})

test('send in a room carries the stay', async () => {
  const { store, sent } = await rig([{ id: 'room-1', name: 'Studio', agents: ['planner'], stay: 7 }])
  await store.refreshMyRooms()
  Object.assign(store, { withLink: async (op: (c: unknown) => Promise<unknown>) => op(store.client) })
  store.send('hi @planner', 'room-1')
  await new Promise((resolve) => setTimeout(resolve, 10))
  assert.equal(sent.length, 1)
  assert.equal(sent[0]!.at(-1), 7)
})
