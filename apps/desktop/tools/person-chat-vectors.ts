// Prints serve/docs/reference/fixtures/person-chat-vectors.json. Run once (the file is the contract, not this script):
//   node apps/desktop/tools/person-chat-vectors.ts > serve/docs/reference/fixtures/person-chat-vectors.json
// Every secret is a fixed test seed derived from a label; the text is made up.
import { x25519 } from '@noble/curves/ed25519.js'
import { sha256 as nobleSha256 } from '@noble/hashes/sha2.js'
import { b64url, signerFromSeed, text, unb64url } from '../src/shared/relay-core/crypto.ts'
import { makeBoxSig, rhashOf, seal, sigInput, aadOf, boxSigInput, type PcEnvelope } from '../src/shared/relay-core/pc.ts'

const seed = (label: string) => nobleSha256(text(`kith person-chat test vector: ${label}`))
const hex = (b: Uint8Array) => Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('')

const sender = signerFromSeed(seed('sender sign'))
const senderKey = b64url(sender.publicKey)
const otherSender = signerFromSeed(seed('sender second device'))
const devA = { sign: signerFromSeed(seed('device A sign')), box: seed('device A box') }
const devB = { sign: signerFromSeed(seed('device B sign')), box: seed('device B box') }
const mac = { box: seed('computer box'), id: 'computer:' + b64url(nobleSha256(text('computer sign')).subarray(0, 16)) }
const rA = b64url(devA.sign.publicKey)
const rB = b64url(devB.sign.publicKey)

const contactId = 'ct_AAECAwQFBgcICQoLDA0ODw'
const epoch = 3
const msgId = 'msg_01J0000000000000000000TEST'
const plaintext = JSON.stringify({ v: 1, text: 'See you at the lake on Saturday', ts: 1796000000000, reply: 'msg_01J0000000000000000000PREV' })
const key = seed('content key K')
const ephemerals = [seed('eph 0'), seed('eph 1'), seed('eph 2')]
const recipients = [
  { r: rB, boxPub: x25519.getPublicKey(devB.box) },
  { r: mac.id, boxPub: x25519.getPublicKey(mac.box) },
  { r: rA, boxPub: x25519.getPublicKey(devA.box) },
]

const env = await seal({ signer: sender, contactId, epoch, msgId, plaintext: text(plaintext), recipients, entropy: { key, ephemerals } })

const jenv = (e: PcEnvelope) => ({
  contact_id: e.contactId, epoch: e.epoch, msg_id: e.msgId, sender_key: e.senderKey, ct: b64url(e.ct), sig: b64url(e.sig), rhash: b64url(e.rhash),
  recipients: e.recipients.map((x) => ({ r: x.r, epk: b64url(x.epk), wrap: b64url(x.wrap) })),
})
const resign = async (e: PcEnvelope): Promise<PcEnvelope> => {
  const rhash = rhashOf(e.recipients)
  return { ...e, rhash, sig: await sender.sign(sigInput(e.contactId, e.epoch, e.msgId, e.senderKey, rhash, e.ct)) }
}

const directory = [senderKey, b64url(otherSender.publicKey)]
const sec = (b: Uint8Array) => b64url(b)
const cases: unknown[] = []
const add = (name: string, r: string, boxSecret: Uint8Array, envelope: PcEnvelope, expect: string, dir = directory) =>
  cases.push({ name, r, box_secret: sec(boxSecret), directory: dir, envelope: jenv(envelope), expect })

add('ok: device A', rA, devA.box, env, 'ok')
add('ok: device B', rB, devB.box, env, 'ok')
add('ok: the computer', mac.id, mac.box, env, 'ok')
add('wrong recipient: R is not in the envelope', b64url(seed('a stranger')), devA.box, env, 'notRecipient')
add('wrong recipient: right R, another device\'s box key', rA, devB.box, env, 'badWrap')
add('sender key is not in the directory', rA, devA.box, env, 'notInDirectory', [b64url(otherSender.publicKey)])
const badRhash = { ...env, rhash: Uint8Array.from(env.rhash, (x, i) => (i === 0 ? x ^ 1 : x)) }
add('tampered rhash', rA, devA.box, badRhash, 'badRhash')
const swapped = { ...env, recipients: env.recipients.map((x, i) => (i === 0 ? { ...x, wrap: Uint8Array.from(x.wrap, (b, j) => (j === 0 ? b ^ 1 : b)) } : x)) }
add('a wrap changed after signing (rhash stale)', rA, devA.box, swapped, 'badRhash')
add('epoch changed, signature not redone', rA, devA.box, { ...env, epoch: 4 }, 'badSignature')
add('wrong-epoch aad: epoch 4 correctly signed over a body sealed for epoch 3', rA, devA.box, await resign({ ...env, epoch: 4 }), 'badCiphertext')
const zeroed = await resign({
  ...env,
  recipients: env.recipients.map((x) => (x.r === rA ? { ...x, epk: new Uint8Array(32), wrap: new Uint8Array(48) } : x)),
})
add('all-zero shared secret: epk is the zero point, everything else correctly signed', rA, devA.box, zeroed, 'zeroSharedSecret')

const userId = 'user_testvector1'
const boxSigOf = async (s: typeof devA.sign, u: string, box: Uint8Array) => b64url(await makeBoxSig(s, u, box))
const goodBox = x25519.getPublicKey(devA.box)
const boxSigs = [
  { name: 'ok', user_id: userId, sign_pub: rA, box_pub: b64url(goodBox), sig: await boxSigOf(devA.sign, userId, goodBox), valid: true },
  { name: 'another user id', user_id: 'user_testvector2', sign_pub: rA, box_pub: b64url(goodBox), sig: await boxSigOf(devA.sign, userId, goodBox), valid: false },
  { name: 'another box key', user_id: userId, sign_pub: rA, box_pub: b64url(x25519.getPublicKey(devB.box)), sig: await boxSigOf(devA.sign, userId, goodBox), valid: false },
  { name: 'signed by another device', user_id: userId, sign_pub: rA, box_pub: b64url(goodBox), sig: await boxSigOf(devB.sign, userId, goodBox), valid: false },
]

const out = {
  _doc: [
    'Person-chat wire vectors (phase4a-build.md section 3). Read by crates/wire/src/pc.rs, KithKit PersonChatCrypto and apps/desktop relay-core/pc.ts.',
    'Encodings: every identifier (contact_id, msg_id, user_id, R, sender_key) is used as its UTF-8 string bytes inside hashes, aads, salts and signature inputs.',
    'R is a device sign_pub as b64url without padding (43 chars) or "computer:<id>"; sender_key is the sender device sign_pub, b64url. epk, wrap, ct, sig, rhash, box_pub, secrets: b64url.',
    'Recipients are sorted by the UTF-8 bytes of R in rhash and in the envelope. u64be(epoch) is 8 bytes big-endian. Nonces are 12 zero bytes (every key is single-use).',
    'The private values are fixed test seeds, not live credentials. The text is made up.',
    'open order of checks, and the error each fails with: directory (notInDirectory), rhash (badRhash), sig (badSignature), own entry (notRecipient), key agreement (zeroSharedSecret), wrap (badWrap), body (badCiphertext).',
    'Seal refusals: no recipients (noRecipients), more than 32 (tooManyRecipients), a repeated R (duplicateRecipient).',
  ],
  keys: {
    sender_sign_seed: b64url(seed('sender sign')), sender_key: senderKey,
    other_sender_key: b64url(otherSender.publicKey),
    device_a: { sign_seed: b64url(seed('device A sign')), r: rA, box_secret: b64url(devA.box), box_pub: b64url(x25519.getPublicKey(devA.box)) },
    device_b: { sign_seed: b64url(seed('device B sign')), r: rB, box_secret: b64url(devB.box), box_pub: b64url(x25519.getPublicKey(devB.box)) },
    computer: { r: mac.id, box_secret: b64url(mac.box), box_pub: b64url(x25519.getPublicKey(mac.box)) },
  },
  seal: {
    contact_id: contactId, epoch, msg_id: msgId, plaintext,
    content_key: b64url(key),
    // In the order the recipients are given to seal: device B, the computer, device A.
    ephemerals: ephemerals.map(b64url),
    recipients: recipients.map((x) => ({ r: x.r, box_pub: b64url(x.boxPub) })),
    aad_hex: hex(aadOf(contactId, epoch, msgId)),
    sig_input_hex: hex(sigInput(contactId, epoch, msgId, senderKey, env.rhash, env.ct)),
    expect: jenv(env),
  },
  box_sig: { input_example_hex: hex(boxSigInput(userId, goodBox)), cases: boxSigs },
  open: cases,
}
void unb64url
console.log(JSON.stringify(out, null, 2))
