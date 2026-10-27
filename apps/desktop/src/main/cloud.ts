import { fromRenderer } from './ipc-guard'
import { createHash, createPrivateKey, createPublicKey, generateKeyPairSync, sign, type KeyObject } from 'node:crypto'
import { promises as fs } from 'node:fs'
import { join } from 'node:path'
import { app, ipcMain, safeStorage } from 'electron'
import type { AccountService } from './account'
import { isInviteProof } from '../shared/invite'
import { signatureHeader as kithSig, type Signer } from '../shared/relay-core/crypto.ts'
import type { CloudFailure, CloudResult } from '../shared/relay'
import { paths, type Requests } from '../shared/cloud-contract.gen.ts'

/**
 * The Kith cloud's user-facing `/v1` API (§8.4) with this install's device key (§3.2): an
 * Ed25519 key per account, kept encrypted with the OS keychain, that signs `Kith-Sig` (§5).
 */

const b64url = (b: Buffer) => b.toString('base64url')

interface Identity {
  key: KeyObject
  publicKey: string
}

const identities = new Map<string, Identity>()

async function identity(userId: string): Promise<Identity> {
  const cached = identities.get(userId)
  if (cached) return cached
  const path = join(app.getPath('userData'), `device-key-${createHash('sha256').update(userId).digest('hex').slice(0, 16)}.bin`)
  let seed: Buffer | null = null
  try {
    const sealed = await fs.readFile(path)
    seed = Buffer.from(safeStorage.isEncryptionAvailable() ? safeStorage.decryptString(sealed) : sealed.toString('utf8'), 'base64url')
  } catch {}
  let key: KeyObject
  if (seed?.length === 32) {
    key = createPrivateKey({ key: { kty: 'OKP', crv: 'Ed25519', d: b64url(seed), x: publicFromSeed(seed) }, format: 'jwk' })
  } else {
    key = generateKeyPairSync('ed25519').privateKey
    const d = (key.export({ format: 'jwk' }) as { d: string }).d
    const raw = safeStorage.isEncryptionAvailable() ? safeStorage.encryptString(d) : Buffer.from(d)
    await fs.mkdir(app.getPath('userData'), { recursive: true })
    await fs.writeFile(path, raw, { mode: 0o600 })
  }
  const publicKey = (key.export({ format: 'jwk' }) as { x: string }).x
  const made = { key, publicKey }
  identities.set(userId, made)
  return made
}

/** The raw Ed25519 public key (b64url) of a 32-byte seed. */
function publicFromSeed(seed: Buffer) {
  // PKCS#8 wrapping of an Ed25519 seed: the fixed 16-byte prefix + the seed.
  const der = Buffer.concat([Buffer.from('302e020100300506032b657004220420', 'hex'), seed])
  const pub = createPublicKey(createPrivateKey({ key: der, format: 'der', type: 'pkcs8' }))
  return (pub.export({ format: 'jwk' }) as { x: string }).x
}

/** The account's device key as the signer the relay client uses. */
const signerOf = (id: Identity): Signer => ({ publicKey: Buffer.from(id.publicKey, 'base64url'), sign: async (m) => sign(null, m, id.key) })

/** `Kith-Sig: v=1,kid,ts,nonce,sig` over method, authority, path and the body's hash. */
async function signatureHeader(id: Identity, method: string, url: URL, body: Buffer) {
  return kithSig(signerOf(id), { method, authority: url.host, pathAndQuery: `${url.pathname || '/'}${url.search}`, body })
}

/** What the relay client needs from the cloud session: signed requests, and the device key to sign the relay upgrade. */
export interface CloudAccess {
  request(method: string, path: string, body: unknown, signed: boolean): Promise<CloudResult<unknown>>
  /** The signed-in account's device key and id; null when signed out. */
  device(): Promise<{ userId: string; signer: Signer; cloudURL: string } | null>
}

export type { CloudFailure }

export function registerCloud(account: AccountService): CloudAccess {
  const send = async (method: string, path: string, body: unknown, signed: boolean): Promise<CloudResult<unknown>> => {
    const base = account.state.cloudURL
    const user = account.state.user
    if (!base || !user) return { ok: false, error: { status: 0, code: 'signedOut', message: 'Sign in again to reach your account.' } satisfies CloudFailure }
    const url = new URL(path.replace(/^\//, ''), `${base}/`)
    const payload = body === undefined ? Buffer.alloc(0) : Buffer.from(JSON.stringify(body))
    const headers: Record<string, string> = {}
    try {
      headers.Authorization = `Bearer ${await account.sessionToken()}`
    } catch (error) {
      return { ok: false, error: { status: 401, code: 'signedOut', message: error instanceof Error ? error.message : 'Sign in again.' } }
    }
    if (body !== undefined) headers['Content-Type'] = 'application/json'
    if (signed) headers['Kith-Sig'] = await signatureHeader(await identity(user.userId), method, url, payload)
    try {
      const res = await fetch(url, { method, headers, body: body === undefined ? undefined : payload, signal: AbortSignal.timeout(20_000) })
      const text = await res.text()
      const json = text ? (JSON.parse(text) as Record<string, unknown>) : {}
      if (!res.ok) {
        const e = (json.error ?? {}) as { code?: string; message?: string }
        return { ok: false, error: { status: res.status, code: e.code ?? `http${res.status}`, message: e.message ?? null } }
      }
      return { ok: true, value: json }
    } catch {
      return { ok: false, error: { status: 0, code: 'unreachable', message: "Can't reach the Kith cloud. Check your connection." } }
    }
  }
  ipcMain.handle('cloud:request', fromRenderer((_e, method: string, path: string, body: unknown, signed: boolean) => send(method, path, body, signed)))
  // Joining by invite link: the cloud gets h1 = sha256(token) and nothing else of the link. The shape is checked here so a
  // renderer slip can't put the token itself on the wire.
  ipcMain.handle('cloud:acceptInvite', (_e, inviteId: string, h1: string) => {
    if (!/^[A-Za-z0-9_-]{1,128}$/.test(inviteId) || !isInviteProof(h1)) {
      return { ok: false, error: { status: 400, code: 'badInvite', message: 'This invite link looks wrong. Ask for a new one.' } }
    }
    return send('POST', paths.acceptInvite(inviteId), { token: h1 } satisfies Requests['acceptInvite'], false)
  })
  return {
    request: send,
    async device() {
      const user = account.state.user
      const cloudURL = account.state.cloudURL
      if (!user || !cloudURL) return null
      return { userId: user.userId, signer: signerOf(await identity(user.userId)), cloudURL }
    },
  }
}
