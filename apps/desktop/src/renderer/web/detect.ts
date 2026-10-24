/**
 * Whether this browser can hold the device key the way the design demands (§4.2): an Ed25519 WebCrypto key that is
 * not extractable and lives in IndexedDB. There is no fallback: a browser without it gets the "too old" screen, never
 * an extractable key or a script-held one.
 */

export type Unsupported = 'webcrypto' | 'ed25519' | 'indexeddb' | 'websocket'

export interface Environment {
  subtle?: SubtleCrypto
  indexedDB?: unknown
  WebSocket?: unknown
  isSecureContext?: boolean
}

export async function detectSupport(env: Environment): Promise<Unsupported | null> {
  const subtle = env.subtle
  if (!subtle || env.isSecureContext === false) return 'webcrypto'
  if (!env.indexedDB) return 'indexeddb'
  if (!env.WebSocket) return 'websocket'
  try {
    // A real round trip: some browsers list the algorithm and fail at use, or hand back an extractable key.
    const pair = (await subtle.generateKey({ name: 'Ed25519' }, false, ['sign', 'verify'])) as CryptoKeyPair
    if (pair.privateKey.extractable) return 'ed25519'
    const message = new Uint8Array([1, 2, 3])
    const sig = await subtle.sign({ name: 'Ed25519' }, pair.privateKey, message)
    if (!(await subtle.verify({ name: 'Ed25519' }, pair.publicKey, sig, message))) return 'ed25519'
    return null
  } catch {
    return 'ed25519'
  }
}

export const detectBrowser = () =>
  detectSupport({
    subtle: globalThis.crypto?.subtle,
    indexedDB: globalThis.indexedDB,
    WebSocket: globalThis.WebSocket,
    isSecureContext: globalThis.isSecureContext,
  })
