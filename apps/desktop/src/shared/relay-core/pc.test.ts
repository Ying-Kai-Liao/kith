import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { join } from 'node:path'
import { test } from 'node:test'
import { x25519 } from '@noble/curves/ed25519.js'
import { b64url, signerFromSeed, text, unb64url } from './crypto.ts'
import { MAX_RECIPIENTS, PcError, aadOf, makeBoxSig, open, rhashOf, seal, sigInput, verifyBoxSig, type PcEnvelope } from './pc.ts'

/* eslint-disable @typescript-eslint/no-explicit-any */
// The same file the Rust (crates/wire/src/pc.rs) and iOS (PersonChatCryptoTests.swift) tests load.
const v: any = JSON.parse(readFileSync(join(import.meta.dirname, '../../../../../serve/docs/reference/fixtures/person-chat-vectors.json'), 'utf8'))
const raw = (s: string) => unb64url(s) as Uint8Array
const hex = (b: Uint8Array) => Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('')

const envelopeOf = (j: any): PcEnvelope => ({
  contactId: j.contact_id, epoch: j.epoch, msgId: j.msg_id, senderKey: j.sender_key, ct: raw(j.ct), sig: raw(j.sig), rhash: raw(j.rhash),
  recipients: j.recipients.map((x: any) => ({ r: x.r, epk: raw(x.epk), wrap: raw(x.wrap) })),
})

const sealInput = () => ({
  signer: signerFromSeed(raw(v.keys.sender_sign_seed)),
  contactId: v.seal.contact_id, epoch: v.seal.epoch, msgId: v.seal.msg_id, plaintext: text(v.seal.plaintext),
  recipients: v.seal.recipients.map((x: any) => ({ r: x.r, boxPub: raw(x.box_pub) })),
  entropy: { key: raw(v.seal.content_key), ephemerals: v.seal.ephemerals.map(raw) },
})

test('vectors: seal reproduces the bytes', async () => {
  const env = await seal(sealInput())
  const want = v.seal.expect
  assert.equal(b64url(env.ct), want.ct)
  assert.equal(b64url(env.rhash), want.rhash)
  assert.equal(b64url(env.sig), want.sig)
  assert.deepEqual(env.recipients.map((x) => ({ r: x.r, epk: b64url(x.epk), wrap: b64url(x.wrap) })), want.recipients)
  assert.equal(hex(aadOf(want.contact_id, want.epoch, want.msg_id)), v.seal.aad_hex)
  assert.equal(hex(sigInput(want.contact_id, want.epoch, want.msg_id, want.sender_key, raw(want.rhash), raw(want.ct))), v.seal.sig_input_hex)
  assert.equal(b64url(rhashOf(envelopeOf(want).recipients)), want.rhash)
})

for (const c of v.open) {
  test(`vectors: open, ${c.name}`, async () => {
    const run = () => open({ r: c.r, boxSecret: raw(c.box_secret), envelope: envelopeOf(c.envelope), directory: c.directory })
    if (c.expect === 'ok') assert.equal(new TextDecoder().decode(await run()), v.seal.plaintext)
    else await assert.rejects(run, (e: unknown) => e instanceof PcError && e.code === c.expect)
  })
}

test('vectors: open with a non-extractable WebCrypto key', async (t) => {
  const c = v.open[0]
  let key: CryptoKey
  try {
    // PKCS8 wrapper for an X25519 private key; importing it as non-extractable is how the web client holds its box key.
    const pkcs8 = new Uint8Array([0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x6e, 0x04, 0x22, 0x04, 0x20, ...raw(c.box_secret)])
    key = await crypto.subtle.importKey('pkcs8', pkcs8, 'X25519', false, ['deriveBits'])
  } catch {
    t.skip('this runtime has no WebCrypto X25519')
    return
  }
  const got = await open({ r: c.r, boxSecret: key, envelope: envelopeOf(c.envelope), directory: c.directory })
  assert.equal(new TextDecoder().decode(got), v.seal.plaintext)
  const zero = v.open.find((x: any) => x.expect === 'zeroSharedSecret')
  await assert.rejects(
    () => open({ r: zero.r, boxSecret: key, envelope: envelopeOf(zero.envelope), directory: zero.directory }),
    (e: unknown) => e instanceof PcError && e.code === 'zeroSharedSecret',
  )
})

for (const c of v.box_sig.cases) {
  test(`vectors: box_sig, ${c.name}`, async () => {
    assert.equal(verifyBoxSig(raw(c.sign_pub), c.user_id, raw(c.box_pub), raw(c.sig)), c.valid)
  })
}

test('makeBoxSig matches the vector', async () => {
  const signer = signerFromSeed(raw(v.keys.device_a.sign_seed))
  const ok = v.box_sig.cases[0]
  assert.equal(b64url(await makeBoxSig(signer, ok.user_id, raw(ok.box_pub))), ok.sig)
})

test('seal refuses no recipients, too many, and a repeated R', async () => {
  const base = sealInput()
  const one = base.recipients[0]
  await assert.rejects(() => seal({ ...base, recipients: [] }), (e: unknown) => e instanceof PcError && e.code === 'noRecipients')
  const many = Array.from({ length: MAX_RECIPIENTS + 1 }, (_, i) => ({ r: `r${i}`, boxPub: one.boxPub }))
  await assert.rejects(() => seal({ ...base, recipients: many, entropy: undefined }), (e: unknown) => e instanceof PcError && e.code === 'tooManyRecipients')
  await assert.rejects(() => seal({ ...base, recipients: [one, one] }), (e: unknown) => e instanceof PcError && e.code === 'duplicateRecipient')
})

test('a fresh seal round trips and never repeats K or the ephemerals', async () => {
  const bob = x25519.getPublicKey(raw(v.keys.device_a.box_secret))
  const signer = signerFromSeed()
  const mk = () => seal({ signer, contactId: 'ct_x', epoch: 1, msgId: 'm1', plaintext: text('hi'), recipients: [{ r: 'R', boxPub: bob }] })
  const [a, b] = [await mk(), await mk()]
  assert.notEqual(b64url(a.recipients[0].epk), b64url(b.recipients[0].epk))
  assert.notEqual(b64url(a.ct), b64url(b.ct))
  const got = await open({ r: 'R', boxSecret: raw(v.keys.device_a.box_secret), envelope: a, directory: [b64url(signer.publicKey)] })
  assert.equal(new TextDecoder().decode(got), 'hi')
})
