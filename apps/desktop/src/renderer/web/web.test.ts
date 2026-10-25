import assert from 'node:assert/strict'
import { webcrypto } from 'node:crypto'
import { test } from 'node:test'
import { b64url, verify, type Signer } from '../../shared/relay-core/crypto.ts'
import { WebCloud } from './cloud.ts'
import { detectSupport } from './detect.ts'
import { appInviteLink, takeInviteFromHash } from './hash.ts'
import { KeyStore, PENDING, type Kv, type Store } from './keystore.ts'
import { emptyAccountText, noticeFor } from './notice.ts'
import { WebRelay, DEVICE_RETIRED } from './relay.ts'
import { KeyRefusal, refusingKeys } from './refusal.ts'
import { signOutInOrder } from './signout.ts'

const subtle = webcrypto.subtle as unknown as SubtleCrypto

// ---- the invite in the address bar ----

const TOKEN = 'T'.repeat(24)
const hashOf = (c: string, i: string, t: string) => `#c=${c}&i=${i}&t=${t}`

function page(hash: string) {
  const replaced: string[] = []
  return {
    replaced,
    location: { hash, pathname: '/join', search: '?x=1' },
    history: { replaceState: (_s: unknown, _u: string, url: string) => void replaced.push(url) },
  }
}

test('the app link is built from the parsed fragment with the build scheme', () => {
  const link = takeInviteFromHash(page(hashOf('cmp_1', 'inv_1', TOKEN)).location, page('').history)!
  assert.equal(appInviteLink('kith-dev', link), `kith-dev://invite?c=cmp_1&i=inv_1&t=${TOKEN}`)
})

test('the invite is read from the fragment and the fragment is dropped from the address bar', () => {
  const p = page(hashOf('cmp_1', 'inv_1', TOKEN))
  assert.deepEqual(takeInviteFromHash(p.location, p.history), { computerId: 'cmp_1', inviteId: 'inv_1', token: TOKEN })
  assert.deepEqual(p.replaced, ['/join?x=1'])
})

test('a link with a missing or odd part gives no invite but still leaves the address bar clean', () => {
  for (const hash of ['#c=cmp_1&i=inv_1', `#c=cmp_1&i=bad/id&t=${TOKEN}`, '#c=cmp_1&i=inv_1&t=short']) {
    const p = page(hash)
    assert.equal(takeInviteFromHash(p.location, p.history), null, hash)
    assert.deepEqual(p.replaced, ['/join?x=1'], hash)
  }
})

test('a page without an invite fragment is left alone', () => {
  for (const hash of ['', '#', '#section']) {
    const p = page(hash)
    assert.equal(takeInviteFromHash(p.location, p.history), null)
    assert.deepEqual(p.replaced, [])
  }
})

// ---- feature detection ----

test('a browser with Ed25519 WebCrypto passes', async () => {
  assert.equal(await detectSupport({ subtle, indexedDB: {}, WebSocket: class {}, isSecureContext: true }), null)
})

test('a browser without Ed25519 gets the too-old verdict, never a fallback', async () => {
  const noEd = { ...subtle, generateKey: async () => { throw new DOMException('Unrecognized name', 'NotSupportedError') } } as unknown as SubtleCrypto
  assert.equal(await detectSupport({ subtle: noEd, indexedDB: {}, WebSocket: class {} }), 'ed25519')
})

test('a browser that hands back an extractable key is refused', async () => {
  const leaky = {
    ...subtle,
    generateKey: async () => ({ privateKey: { extractable: true }, publicKey: {} }),
  } as unknown as SubtleCrypto
  assert.equal(await detectSupport({ subtle: leaky, indexedDB: {}, WebSocket: class {} }), 'ed25519')
})

test('missing WebCrypto, an insecure page or missing IndexedDB each name what is missing', async () => {
  assert.equal(await detectSupport({ indexedDB: {}, WebSocket: class {} }), 'webcrypto')
  assert.equal(await detectSupport({ subtle, indexedDB: {}, WebSocket: class {}, isSecureContext: false }), 'webcrypto')
  assert.equal(await detectSupport({ subtle, WebSocket: class {} }), 'indexeddb')
})

// ---- the key store ----

function memoryKv() {
  const data = new Map<Store, Map<string, unknown>>([['keys', new Map()], ['wrap', new Map()], ['invites', new Map()]])
  const kv: Kv = {
    get: async (s, k) => data.get(s)!.get(k),
    put: async (s, k, v) => void data.get(s)!.set(k, v),
    delete: async (s, k) => void data.get(s)!.delete(k),
    keys: async (s) => [...data.get(s)!.keys()],
  }
  return { kv, data }
}

const newStore = () => {
  const m = memoryKv()
  return { ...m, store: new KeyStore(m.kv, subtle, (_n, fn) => fn()) }
}

test('the device key is made once per user, non-extractable, and signs verifiably', async () => {
  const { store, data } = newStore()
  assert.equal(await store.identity('user_a', false), null)
  const a = (await store.identity('user_a', true))!
  const again = (await store.identity('user_a', true))!
  assert.deepEqual(a.publicKey, again.publicKey)
  assert.equal(a.publicKey.length, 32)
  const stored = data.get('keys')!.get('user_a') as { privateKey: CryptoKey }
  assert.equal(stored.privateKey.extractable, false)
  await assert.rejects(subtle.exportKey('raw', stored.privateKey))
  const message = new Uint8Array([9, 8, 7])
  assert.ok(verify(a.publicKey, await a.sign(message), message))
  // Another account on the same browser gets its own key.
  const b = (await store.identity('user_b', true))!
  assert.notDeepEqual(a.publicKey, b.publicKey)
})

test('a key the browser handed back extractable is never used', async () => {
  const { store, data } = newStore()
  data.get('keys')!.set('user_a', { privateKey: { extractable: true }, publicKey: {}, raw: new Uint8Array(32) })
  assert.equal(await store.identity('user_a', false), null)
})

test('a kept invite is sealed in the store and bound to its slot', async () => {
  const { store, data } = newStore()
  await store.keepInvite('user_a/cmp_1', { inviteId: 'inv_1', token: TOKEN })
  const sealed = data.get('invites')!.get('user_a/cmp_1') as { iv: Uint8Array; data: ArrayBuffer }
  assert.ok(!Buffer.from(sealed.data).includes(Buffer.from(TOKEN)), 'the token is not in the clear')
  assert.deepEqual(await store.keptInvite('user_a/cmp_1'), { inviteId: 'inv_1', token: TOKEN })
  // Moved to another slot it does not open: the slot is authenticated data.
  data.get('invites')!.set('user_b/cmp_1', sealed)
  assert.equal(await store.keptInvite('user_b/cmp_1'), null)
  assert.equal(await store.hasInvite('user_a/cmp_1'), true)
  await store.forgetInvite('user_a/cmp_1')
  assert.equal(await store.hasInvite('user_a/cmp_1'), false)
})

test('a link waiting for sign-in survives until it is cleared, and sign-out wipes key and tokens', async () => {
  const { store, data } = newStore()
  await store.savePending({ computerId: 'cmp_1', inviteId: 'inv_1', token: TOKEN })
  assert.deepEqual(await store.pending(), { computerId: 'cmp_1', inviteId: 'inv_1', token: TOKEN })
  assert.deepEqual(await store.pending(), { computerId: 'cmp_1', inviteId: 'inv_1', token: TOKEN }, 'a reload mid-join finds it again')
  await store.identity('user_a', true)
  await store.identity('user_b', true)
  await store.keepInvite('user_a/cmp_1', { inviteId: 'inv_1', token: TOKEN })
  await store.keepInvite('user_b/cmp_2', { inviteId: 'inv_2', token: TOKEN })
  await store.wipe('user_a')
  assert.equal(await store.identity('user_a', false), null)
  assert.equal(await store.keptInvite('user_a/cmp_1'), null)
  assert.equal(await store.pending(), null)
  assert.equal(data.get('invites')!.has(PENDING), false)
  // The other account's key and token stay.
  assert.ok(await store.identity('user_b', false))
  assert.ok(await store.keptInvite('user_b/cmp_2'))
})

// ---- sign-out order (design §4.2) ----

function steps(over: Partial<Parameters<typeof signOutInOrder>[0]> = {}) {
  const log: string[] = []
  const signer: Signer = { publicKey: new Uint8Array(32), sign: async () => new Uint8Array(64) }
  const s = {
    halt: () => void log.push('halt'),
    closeLinks: () => void log.push('close'),
    signer: async () => (log.push('signer'), signer),
    retire: async () => (log.push('retire'), { ok: true }),
    wipe: async () => void log.push('wipe'),
    endSession: async () => void log.push('end'),
    ...over,
  }
  return { log, s }
}

test('sign-out aborts and closes, then retires, then deletes, then ends the session', async () => {
  const { log, s } = steps()
  const res = await signOutInOrder(s)
  assert.deepEqual(log, ['halt', 'close', 'signer', 'retire', 'wipe', 'end'])
  assert.equal(res.retired, true)
})

test('sign-out still deletes the key and ends the session when the retire fails offline', async () => {
  for (const retire of [async () => ({ ok: false }), async () => { throw new Error('offline') }]) {
    const { log, s } = steps({ retire })
    const res = await signOutInOrder(s)
    assert.deepEqual(log, ['halt', 'close', 'signer', 'wipe', 'end'])
    assert.equal(res.retired, false)
  }
})

test('sign-out with no key has nothing to retire, and a failing delete does not keep the session alive', async () => {
  const none = steps({ signer: async () => null })
  await signOutInOrder(none.s)
  assert.deepEqual(none.log, ['halt', 'close', 'wipe', 'end'])
  const broken = steps({ wipe: async () => { throw new Error('idb') } })
  await assert.rejects(signOutInOrder(broken.s))
  assert.deepEqual(broken.log, ['halt', 'close', 'signer', 'retire', 'end'])
})

// ---- the cloud client: abort in flight, signed requests ----

const fakeSigner = async () => (await new KeyStore(memoryKv().kv, subtle, (_n, fn) => fn()).identity('u', true))!

test('halt aborts a request in flight and refuses new ones; a request still being signed never reaches the network', async () => {
  const signer = await fakeSigner()
  let fetched = 0
  let release!: () => void
  const gate = new Promise<void>((r) => (release = r))
  const cloud = new WebCloud({
    cloudURL: 'https://cloud.test',
    token: async () => (await gate, 'jwt'),
    signer: async () => signer,
    fetch: (async () => (fetched++, new Response('{}'))) as typeof fetch,
  })
  const early = cloud.request('POST', 'v1/devices', { name: 'x' }, true)
  cloud.halt()
  release()
  const res = await early
  assert.equal(res.ok, false)
  assert.equal(fetched, 0, 'nothing signed before sign-out can land after it')
  const later = await cloud.request('GET', 'v1/computers', undefined, true)
  assert.equal(later.ok === false && later.error.code, 'signedOut')
  cloud.resume()
  assert.equal((await cloud.request('GET', 'v1/computers', undefined, false)).ok, true)
})

test('a signed request carries the Bearer JWT and a Kith-Sig the key made; retire is signed and ignores halt', async () => {
  const signer = await fakeSigner()
  const seen: { url: string; headers: Record<string, string> }[] = []
  const cloud = new WebCloud({
    cloudURL: 'https://cloud.test/',
    token: async () => 'jwt-1',
    signer: async () => signer,
    fetch: (async (url: URL, init: RequestInit) => (seen.push({ url: String(url), headers: init.headers as Record<string, string> }), new Response('{}'))) as unknown as typeof fetch,
  })
  await cloud.request('POST', 'v1/devices', { name: 'x', platform: 'web' }, true)
  assert.equal(seen[0]!.url, 'https://cloud.test/v1/devices')
  assert.equal(seen[0]!.headers.Authorization, 'Bearer jwt-1')
  assert.match(seen[0]!.headers['Kith-Sig']!, new RegExp(`^v=1,kid=${b64url(signer.publicKey)},ts=\\d+,nonce=`))
  cloud.halt()
  assert.equal((await cloud.retire(signer)).ok, true)
  assert.equal(seen[1]!.url, 'https://cloud.test/v1/devices/retire')
  assert.ok(seen[1]!.headers['Kith-Sig'])
  assert.equal(seen[1]!.headers.Authorization, undefined, 'retire needs no login')
})

// ---- registration: one per tab, and a refused key is dropped ----

function relayWith(register: () => Promise<unknown>, dropped: string[]) {
  let calls = 0
  const cloud = {
    request: async (method: string, path: string) => {
      if (path === 'v1/devices') {
        calls++
        return register()
      }
      throw new Error(`${method} ${path}`)
    },
  } as unknown as WebCloud
  const relay = new WebRelay({
    cloud,
    cloudURL: 'https://cloud.test',
    session: () => ({ userId: 'u', sid: 'sess_1' }),
    jwt: async () => 'jwt',
    signer: async () => null,
    keyRefused: async (u) => void dropped.push(u),
    deviceName: 'Kith web',
  })
  return { relay, calls: () => calls }
}

test('two callers racing to register share one request', async () => {
  const { relay, calls } = relayWith(async () => ({ ok: true, value: {} }), [])
  const [a, b] = await Promise.all([relay.ensureDevice(), relay.ensureDevice()])
  assert.equal(a.ok && b.ok, true)
  assert.equal(calls(), 1)
  await relay.ensureDevice()
  assert.equal(calls(), 1, 'and a later call does not register again in the same session')
})

test('a refused key (403) is dropped so the next load makes a new device, and the person is told', async () => {
  const dropped: string[] = []
  const { relay } = relayWith(async () => ({ ok: false, error: { status: 403, code: 'forbidden', message: 'retired' } }), dropped)
  const res = await relay.ensureDevice()
  assert.equal(res.ok, false)
  assert.equal(!res.ok && res.error.message, DEVICE_RETIRED)
  assert.deepEqual(dropped, ['u'])
})

test('after the cloud refuses a key, no new key is made in this page for that user', () => {
  const r = new KeyRefusal()
  assert.equal(r.mayCreate('u'), true)
  r.refuse('u')
  assert.equal(r.mayCreate('u'), false)
  assert.equal(r.mayCreate('other'), true)
})

test('wired as in the bridge: after a 403 the cloud signer and the relay signer make and register no new key in this page', async () => {
  const held = new Map<string, Signer>([['u', await fakeSigner()]])
  let created = 0
  const store = {
    identity: async (user: string, create: boolean) => {
      const have = held.get(user)
      if (have || !create) return have ?? null
      created++
      const made = await fakeSigner()
      held.set(user, made)
      return made
    },
    dropKey: async (user: string) => void held.delete(user),
  }
  const refusing = refusingKeys(store, () => 'u')
  let posts = 0
  const cloud = new WebCloud({
    cloudURL: 'https://cloud.test',
    token: async () => 'jwt',
    signer: refusing.cloudSigner,
    fetch: (async () => (posts++, new Response('{"error":"forbidden"}', { status: 403 }))) as unknown as typeof fetch,
  })
  const relay = new WebRelay({
    cloud,
    cloudURL: 'https://cloud.test',
    session: () => ({ userId: 'u', sid: 'sess_1' }),
    jwt: async () => 'jwt',
    signer: refusing.relaySigner,
    keyRefused: refusing.refused,
    deviceName: 'Kith web',
  })
  const first = await relay.ensureDevice()
  assert.equal(!first.ok && first.error.message, DEVICE_RETIRED)
  assert.equal(held.has('u'), false, 'the refused key is dropped')
  const again = await relay.ensureDevice()
  assert.equal(again.ok, false)
  assert.equal(await refusing.relaySigner(true), null, 'even a caller that asks to create gets no key')
  assert.equal(created, 0, 'no new key was made')
  assert.equal(posts, 1, 'and nothing was registered a second time')
})

test('before any refusal a missing key may still be made', async () => {
  let created = 0
  const refusing = refusingKeys({ identity: async (_u: string, create: boolean) => (create ? (created++, 'k') : null), dropKey: async () => {} }, () => 'u')
  assert.equal(await refusing.relaySigner(true), 'k')
  assert.equal(created, 1)
})

test('a link that was past registration when sign-out closed everything is not created', async () => {
  let release!: () => void
  const gate = new Promise<void>((r) => (release = r))
  const cloud = { request: async () => (await gate, { ok: true, value: {} }) } as unknown as WebCloud
  const signer: Signer = { publicKey: new Uint8Array(32), sign: async () => new Uint8Array(64) }
  const relay = new WebRelay({
    cloud,
    cloudURL: 'https://cloud.test',
    session: () => ({ userId: 'u', sid: 's' }),
    jwt: async () => 'jwt',
    signer: async () => signer,
    keyRefused: async () => {},
    deviceName: 'Kith web',
  })
  const pending = relay.bridge.state('cmp', 'AAAA')
  relay.closeAll()
  release()
  const state = await pending
  assert.equal(state.kind, 'failed')
  assert.equal(state.kind === 'failed' && state.code, 'signedOut')
})

test('a link waiting for sign-in belongs to the first account that signs in with it', async () => {
  const { store } = newStore()
  await store.savePending({ computerId: 'cmp_1', inviteId: 'inv_1', token: TOKEN })
  assert.equal((await store.pending(null))?.forUser ?? null, null, 'signed out: nobody owns it yet')
  assert.ok(await store.pending('user_a'))
  assert.ok(await store.pending('user_a'), 'a reload for the same account still finds it')
  assert.equal(await store.pending('user_b'), null, 'another account never gets it')
  assert.equal(await store.pending('user_a'), null, 'and it is gone')
})

// ---- link outcome to words ----

test('link outcomes become the words and the action the page shows', () => {
  assert.deepEqual(noticeFor({ kind: 'ready' }, 'Kai MacBook'), { kind: 'none' })
  assert.deepEqual(noticeFor(null, 'x'), { kind: 'none' })
  assert.equal(noticeFor({ kind: 'hostOffline', lastSeenAt: null }, "Kai's MacBook").kind, 'info')
  assert.match((noticeFor({ kind: 'hostOffline', lastSeenAt: null }, "Kai's MacBook") as { text: string }).text, /^Kai's MacBook is offline/)
  assert.equal(noticeFor({ kind: 'failed', message: 'Sign in again to reach your computers.', code: 'signedOut', final: true }, 'x').kind, 'signIn')
  assert.equal(noticeFor({ kind: 'unauthorized', message: 'Your access to this computer was removed.' }, 'x').kind, 'removed')
  assert.equal(noticeFor({ kind: 'unauthorized', message: 'm', retrying: true }, 'x').kind, 'info')
  assert.equal(noticeFor({ kind: 'failed', message: 'This isn', code: 'unknownComputer', final: true }, 'x').kind, 'removed')
  assert.equal(noticeFor({ kind: 'failed', message: DEVICE_RETIRED, code: 'deviceRetired', final: false }, 'x').kind, 'removed')
  assert.equal(noticeFor({ kind: 'failed', message: 'update', code: 'upgrade', final: true }, 'x').kind, 'error')
  assert.equal(noticeFor({ kind: 'failed', message: "Can't reach", code: 'unreachable', final: false }, 'x').kind, 'info')
})

test('an empty account is told what the web is for: owners to the app, others to an invite', () => {
  assert.match(emptyAccountText([{ role: 'owner' }]), /for people you invite.*Kith app/)
  assert.equal(emptyAccountText([]), "Open an invite link to join someone's Kith.")
})
