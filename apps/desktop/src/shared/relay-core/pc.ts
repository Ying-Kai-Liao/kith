import { chacha20poly1305 } from '@noble/ciphers/chacha.js'
import { x25519 } from '@noble/curves/ed25519.js'
import { hkdf } from '@noble/hashes/hkdf.js'
import { sha256 as nobleSha256 } from '@noble/hashes/sha2.js'
import { b64url, bytesEqual, cat, randomBytes, text, unb64url, verify, type Signer } from './crypto.ts'

/**
 * Person-chat wire crypto (designs/2026-11-24-app-to-design/phase4a-build.md §3). One random content key K seals the
 * body once; only K is wrapped per recipient device (X25519 + HKDF-SHA256 + ChaCha20-Poly1305), and the sender signs
 * what the cloud and every recipient can recompute. The same construction as crates/wire/src/pc.rs and KithKit's
 * PersonChatCrypto.swift, all held to serve/docs/reference/fixtures/person-chat-vectors.json.
 *
 * Reusable by the cloud (Workers): `rhashOf`, `sigInput`, `aadOf`, `boxSigInput` and `verifyBoxSig` need no private
 * key and no WebCrypto, so the cloud imports this file (or copies those pieces) to recompute rhash and verify `sig`
 * at send.
 *
 * Encodings, fixed here once: every identifier is the UTF-8 bytes of its string. A recipient key R is the device's
 * sign_pub as b64url without padding (43 chars) or `computer:<id>`; the sender key is the sender device's sign_pub in
 * the same b64url form. epk, wrap, ct, sig and rhash travel as b64url; epk and box_pub are raw 32 bytes inside hashes.
 * Every label starts `kith/pc`, distinct from the mailbox family.
 *
 * The zero nonce is safe only because every key it is used with is single-use: K is fresh per message and each wrap
 * key derives from a fresh ephemeral per recipient per message. Never seal twice under one K or one e_i.
 */

const ZERO_NONCE = new Uint8Array(12)
/** The cloud's limit on recipient devices per message. */
export const MAX_RECIPIENTS = 32

export type PcErrorCode =
  | 'noRecipients' | 'tooManyRecipients' | 'duplicateRecipient'
  | 'notInDirectory' | 'badRhash' | 'badSignature' | 'notRecipient'
  | 'zeroSharedSecret' | 'badWrap' | 'badCiphertext'

export class PcError extends Error {
  readonly code: PcErrorCode
  constructor(code: PcErrorCode) {
    super(code)
    this.code = code
  }
}

export interface PcRecipient {
  /** Device sign_pub (b64url) or `computer:<id>`. */
  r: string
  epk: Uint8Array
  wrap: Uint8Array
}

export interface PcEnvelope {
  contactId: string
  epoch: number
  msgId: string
  /** The sender device's sign_pub, b64url. */
  senderKey: string
  ct: Uint8Array
  sig: Uint8Array
  rhash: Uint8Array
  recipients: PcRecipient[]
}

const u64be = (n: number) => {
  const out = new Uint8Array(8)
  new DataView(out.buffer).setBigUint64(0, BigInt(n))
  return out
}

const byUtf8 = (a: string, b: string) => {
  const x = text(a)
  const y = text(b)
  for (let i = 0; i < Math.min(x.length, y.length); i++) if (x[i] !== y[i]) return x[i] - y[i]
  return x.length - y.length
}

// ---- pieces the cloud recomputes ----

/** The content AEAD's aad. */
export const aadOf = (contactId: string, epoch: number, msgId: string) =>
  cat(text('kith/pc-msg/v1'), text(contactId), u64be(epoch), text(msgId))

/** SHA-256 over the recipients sorted by R (UTF-8 byte order): R ‖ epk ‖ wrap for each. */
export function rhashOf(recipients: readonly PcRecipient[]): Uint8Array {
  const sorted = [...recipients].sort((a, b) => byUtf8(a.r, b.r))
  return nobleSha256(cat(...sorted.flatMap((x) => [text(x.r), x.epk, x.wrap])))
}

/** What the sender signs and the cloud verifies at send. */
export const sigInput = (contactId: string, epoch: number, msgId: string, senderKey: string, rhash: Uint8Array, ct: Uint8Array) =>
  cat(text('kith/pc/v1'), text(contactId), u64be(epoch), text(msgId), text(senderKey), rhash, nobleSha256(ct))

/** What a device's sign key signs to vouch for its box key. */
export const boxSigInput = (userId: string, boxPub: Uint8Array) => cat(text('kith/pc-box/v1'), text(userId), boxPub)

export const makeBoxSig = (signer: Signer, userId: string, boxPub: Uint8Array) => signer.sign(boxSigInput(userId, boxPub))

export const verifyBoxSig = (signPub: Uint8Array, userId: string, boxPub: Uint8Array, sig: Uint8Array) =>
  verify(signPub, sig, boxSigInput(userId, boxPub))

// ---- key schedule ----

const wrapKey = (msgId: string, r: string, epk: Uint8Array, ss: Uint8Array) =>
  hkdf(nobleSha256, ss, cat(text('kith/pc-wrap/v1'), text(msgId), text(r), epk), text('kith/pc-wrap-key/v1'), 32)

const wrapAad = (msgId: string, r: string) => cat(text(msgId), text(r))

function dh(priv: Uint8Array, pub: Uint8Array): Uint8Array {
  let ss: Uint8Array
  try {
    ss = x25519.getSharedSecret(priv, pub)
  } catch {
    throw new PcError('zeroSharedSecret')
  }
  if (ss.every((b) => b === 0)) throw new PcError('zeroSharedSecret')
  return ss
}

// ---- seal ----

export interface SealInput {
  signer: Signer
  contactId: string
  epoch: number
  msgId: string
  plaintext: Uint8Array
  /** `r` as above; `boxPub` the device's raw X25519 key (its box_sig is checked by the caller). */
  recipients: readonly { r: string; boxPub: Uint8Array }[]
  /** Fixed randomness for the vectors only: K, and one ephemeral private key per recipient in the order given. */
  entropy?: { key: Uint8Array; ephemerals: Uint8Array[] }
}

/** The sender key is `b64url(signer.publicKey)`. Returns the envelope with recipients sorted by R. */
export async function seal(i: SealInput): Promise<PcEnvelope> {
  if (i.recipients.length === 0) throw new PcError('noRecipients')
  if (i.recipients.length > MAX_RECIPIENTS) throw new PcError('tooManyRecipients')
  if (new Set(i.recipients.map((x) => x.r)).size !== i.recipients.length) throw new PcError('duplicateRecipient')
  const k = i.entropy?.key ?? randomBytes(32)
  const ct = chacha20poly1305(k, ZERO_NONCE, aadOf(i.contactId, i.epoch, i.msgId)).encrypt(i.plaintext)
  const recipients = i.recipients.map((x, n) => {
    const e = i.entropy?.ephemerals[n] ?? randomBytes(32)
    const epk = x25519.getPublicKey(e)
    const key = wrapKey(i.msgId, x.r, epk, dh(e, x.boxPub))
    return { r: x.r, epk, wrap: chacha20poly1305(key, ZERO_NONCE, wrapAad(i.msgId, x.r)).encrypt(k) }
  })
  recipients.sort((a, b) => byUtf8(a.r, b.r))
  const rhash = rhashOf(recipients)
  const senderKey = b64url(i.signer.publicKey)
  const sig = await i.signer.sign(sigInput(i.contactId, i.epoch, i.msgId, senderKey, rhash, ct))
  return { contactId: i.contactId, epoch: i.epoch, msgId: i.msgId, senderKey, ct, sig, rhash, recipients }
}

// ---- open ----

export interface OpenInput {
  /** The recipient key R this device was sealed under. */
  r: string
  /** The device's box private key: raw 32 bytes (tests), or a non-extractable WebCrypto X25519 CryptoKey. */
  boxSecret: Uint8Array | CryptoKey
  envelope: PcEnvelope
  /** The sender account's device sign_pub keys (b64url) from the contact's directory. */
  directory: readonly string[]
}

async function agree(secret: Uint8Array | CryptoKey, epk: Uint8Array): Promise<Uint8Array> {
  if (secret instanceof Uint8Array) return dh(secret, epk)
  try {
    const pub = await crypto.subtle.importKey('raw', epk as BufferSource, 'X25519', false, [])
    const ss = new Uint8Array(await crypto.subtle.deriveBits({ name: 'X25519', public: pub }, secret, 256))
    if (ss.every((b) => b === 0)) throw new PcError('zeroSharedSecret')
    return ss
  } catch (e) {
    // WebCrypto itself refuses a low-order point.
    throw e instanceof PcError ? e : new PcError('zeroSharedSecret')
  }
}

/** Checks directory, rhash and sig, then unwraps K and opens the body. Throws PcError; returns the plaintext bytes. */
export async function open(i: OpenInput): Promise<Uint8Array> {
  const e = i.envelope
  if (!i.directory.includes(e.senderKey)) throw new PcError('notInDirectory')
  if (!bytesEqual(rhashOf(e.recipients), e.rhash)) throw new PcError('badRhash')
  const senderPub = unb64url(e.senderKey)
  if (!senderPub || senderPub.length !== 32 || !verify(senderPub, e.sig, sigInput(e.contactId, e.epoch, e.msgId, e.senderKey, e.rhash, e.ct)))
    throw new PcError('badSignature')
  const mine = e.recipients.find((x) => x.r === i.r)
  if (!mine) throw new PcError('notRecipient')
  const key = wrapKey(e.msgId, mine.r, mine.epk, await agree(i.boxSecret, mine.epk))
  let k: Uint8Array
  try {
    k = chacha20poly1305(key, ZERO_NONCE, wrapAad(e.msgId, mine.r)).decrypt(mine.wrap)
  } catch {
    throw new PcError('badWrap')
  }
  try {
    return chacha20poly1305(k, ZERO_NONCE, aadOf(e.contactId, e.epoch, e.msgId)).decrypt(e.ct)
  } catch {
    throw new PcError('badCiphertext')
  }
}
