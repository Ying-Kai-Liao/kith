import { b64url, randomBytes, text, type Signer } from '../../shared/relay-core/crypto.ts'

/**
 * What the web client keeps in this browser (design §4.2), in IndexedDB on the page's own origin:
 *  - `keys`: the device identity, an Ed25519 CryptoKeyPair made with `extractable: false`, one per Clerk user id.
 *    Script can sign with it while the page runs but can never read the private key out.
 *  - `wrap`: one non-extractable AES-GCM key that seals the invite token `t` (and a link waiting for sign-in).
 *    That keeps `t` off the disk in the clear; it does not hide it from the page itself.
 *  - `invites`: the sealed `{inviteId, token}` per `<userId>/<computerId>`, plus one `pending` slot for a link that
 *    arrived before sign-in (the sign-in redirect leaves the page, so the link must survive it).
 * The key is never exported and there is no fallback that makes it extractable: `identity` throws if the browser hands
 * back an extractable private key.
 */

export type Store = 'keys' | 'wrap' | 'invites'

export interface Kv {
  get(store: Store, key: string): Promise<unknown>
  put(store: Store, key: string, value: unknown): Promise<void>
  delete(store: Store, key: string): Promise<void>
  keys(store: Store): Promise<string[]>
}

interface StoredIdentity {
  privateKey: CryptoKey
  publicKey: CryptoKey
  raw: Uint8Array
  createdAt: number
}

interface Sealed {
  iv: Uint8Array
  data: ArrayBuffer
}

export interface KeptInvite {
  inviteId: string
  token: string
}

export interface PendingLink {
  computerId: string
  inviteId: string
  token: string
  /** The Clerk user who first signed in with it waiting: nobody else may use it (null until then). */
  forUser?: string | null
}

export const PENDING = 'pending'

/** Runs `fn` while holding a cross-tab lock where the browser has Web Locks; without them it just runs. */
export type WithLock = <T>(name: string, fn: () => Promise<T>) => Promise<T>

export const browserLock: WithLock = (name, fn) => {
  const locks = (globalThis.navigator as Navigator | undefined)?.locks
  return locks ? locks.request(name, fn) : fn()
}

export class KeyStore {
  private readonly kv: Kv
  private readonly subtle: SubtleCrypto
  private readonly lock: WithLock

  constructor(kv: Kv, subtle: SubtleCrypto = globalThis.crypto.subtle, lock: WithLock = browserLock) {
    this.kv = kv
    this.subtle = subtle
    this.lock = lock
  }

  /**
   * The signer for a Clerk user's device key. With `create` it makes the key the first time; without, a user who has no
   * key yet gets null. Creation holds a lock so two tabs cannot each make a key and keep different ones.
   */
  async identity(userId: string, create: boolean): Promise<Signer | null> {
    const found = await this.load(userId)
    if (found || !create) return found ? this.signer(found) : null
    return this.lock(`kith-key:${userId}`, async () => {
      const again = await this.load(userId)
      if (again) return this.signer(again)
      const pair = (await this.subtle.generateKey({ name: 'Ed25519' }, false, ['sign', 'verify'])) as CryptoKeyPair
      if (pair.privateKey.extractable) throw new Error('This browser made an extractable key; Kith will not use it.')
      const raw = new Uint8Array(await this.subtle.exportKey('raw', pair.publicKey))
      const made: StoredIdentity = { privateKey: pair.privateKey, publicKey: pair.publicKey, raw, createdAt: Date.now() }
      await this.kv.put('keys', userId, made)
      return this.signer(made)
    })
  }

  private async load(userId: string): Promise<StoredIdentity | null> {
    const v = (await this.kv.get('keys', userId)) as StoredIdentity | undefined
    if (!v?.privateKey || v.privateKey.extractable) return null
    return v
  }

  private signer(id: StoredIdentity): Signer {
    return {
      publicKey: id.raw,
      sign: async (message) => new Uint8Array(await this.subtle.sign({ name: 'Ed25519' }, id.privateKey, message as BufferSource)),
    }
  }

  // ---- kept invites ----

  private async wrapKey(): Promise<CryptoKey> {
    const have = (await this.kv.get('wrap', 'aes')) as CryptoKey | undefined
    if (have) return have
    return this.lock('kith-wrap', async () => {
      const again = (await this.kv.get('wrap', 'aes')) as CryptoKey | undefined
      if (again) return again
      const key = await this.subtle.generateKey({ name: 'AES-GCM', length: 256 }, false, ['encrypt', 'decrypt'])
      await this.kv.put('wrap', 'aes', key)
      return key
    })
  }

  private async seal(slot: string, value: unknown): Promise<Sealed> {
    const iv = randomBytes(12)
    const data = await this.subtle.encrypt({ name: 'AES-GCM', iv: iv as BufferSource, additionalData: text(slot) as BufferSource }, await this.wrapKey(), text(JSON.stringify(value)) as BufferSource)
    return { iv, data }
  }

  private async open<T>(slot: string, sealed: Sealed | undefined): Promise<T | null> {
    if (!sealed?.iv || !sealed.data) return null
    try {
      const plain = await this.subtle.decrypt({ name: 'AES-GCM', iv: sealed.iv as BufferSource, additionalData: text(slot) as BufferSource }, await this.wrapKey(), sealed.data)
      return JSON.parse(new TextDecoder().decode(plain)) as T
    } catch {
      return null
    }
  }

  async keepInvite(slot: string, invite: KeptInvite): Promise<void> {
    await this.kv.put('invites', slot, await this.seal(slot, invite))
  }

  async keptInvite(slot: string): Promise<KeptInvite | null> {
    return this.open<KeptInvite>(slot, (await this.kv.get('invites', slot)) as Sealed | undefined)
  }

  async hasInvite(slot: string): Promise<boolean> {
    return (await this.kv.get('invites', slot)) !== undefined
  }

  async forgetInvite(slot: string): Promise<void> {
    await this.kv.delete('invites', slot)
  }

  // ---- a link that arrived before sign-in ----

  async savePending(link: PendingLink): Promise<void> {
    await this.kv.put('invites', PENDING, await this.seal(PENDING, link))
  }

  /**
   * The waiting link, left in place: a reload mid-join resumes from it. `clearPending` removes it once the join is kept.
   * With a user, the first one to ask owns it; a link that belongs to someone else is dropped, so signing in as another
   * account never joins with the earlier person's invite.
   */
  async pending(userId: string | null = null): Promise<PendingLink | null> {
    const link = await this.open<PendingLink>(PENDING, (await this.kv.get('invites', PENDING)) as Sealed | undefined)
    if (!link || userId === null) return link
    if (link.forUser && link.forUser !== userId) {
      await this.clearPending()
      return null
    }
    if (!link.forUser) await this.savePending({ ...link, forUser: userId })
    return { ...link, forUser: userId }
  }

  async clearPending(): Promise<void> {
    await this.kv.delete('invites', PENDING)
  }

  /** The user's key only (the cloud refused it); kept invites stay so the next key can prove the same invite again. */
  async dropKey(userId: string): Promise<void> {
    await this.kv.delete('keys', userId)
  }

  // ---- sign-out ----

  /** Deletes the user's device key and every token kept for them (§4.2 step 3). The pending link goes too. */
  async wipe(userId: string): Promise<void> {
    await this.kv.delete('keys', userId)
    for (const slot of await this.kv.keys('invites')) if (slot === PENDING || slot.startsWith(`${userId}/`)) await this.kv.delete('invites', slot)
  }
}

// ---- IndexedDB ----

const DB = 'kith-web'

function open(idb: IDBFactory): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const req = idb.open(DB, 1)
    req.onupgradeneeded = () => {
      for (const s of ['keys', 'wrap', 'invites'] as const) if (!req.result.objectStoreNames.contains(s)) req.result.createObjectStore(s)
    }
    req.onsuccess = () => resolve(req.result)
    req.onerror = () => reject(req.error ?? new Error('IndexedDB is not available'))
  })
}

/** The browser's IndexedDB as a `Kv`. Structured clone keeps CryptoKeys opaque and non-extractable. */
export function indexedDbKv(idb: IDBFactory = globalThis.indexedDB): Kv {
  let db: Promise<IDBDatabase> | null = null
  const run = async <T>(store: Store, mode: IDBTransactionMode, fn: (s: IDBObjectStore) => IDBRequest<T>): Promise<T> => {
    db ??= open(idb)
    const conn = await db
    return new Promise<T>((resolve, reject) => {
      const tx = conn.transaction(store, mode)
      const req = fn(tx.objectStore(store))
      tx.oncomplete = () => resolve(req.result)
      tx.onerror = () => reject(tx.error)
      tx.onabort = () => reject(tx.error)
    })
  }
  return {
    get: (store, key) => run(store, 'readonly', (s) => s.get(key)),
    put: async (store, key, value) => void (await run(store, 'readwrite', (s) => s.put(value, key))),
    delete: async (store, key) => void (await run(store, 'readwrite', (s) => s.delete(key))),
    keys: async (store) => (await run(store, 'readonly', (s) => s.getAllKeys())).map(String),
  }
}

/** Asks the browser not to evict this origin's storage (Safari and Chrome may otherwise clear an idle site's key). */
export async function requestPersistence(): Promise<void> {
  try {
    await navigator.storage?.persist?.()
  } catch {}
}

export const publicKeyB64 = (signer: Signer) => b64url(signer.publicKey)
