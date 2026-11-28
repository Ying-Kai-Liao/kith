import CryptoKit
import Foundation

/// Person-chat wire crypto (designs/2026-11-24-app-to-design/phase4a-build.md §3). One random content key K seals the
/// body once; only K is wrapped per recipient device (X25519 + HKDF-SHA256 + ChaCha20-Poly1305), and the sender signs
/// what the cloud and every recipient can recompute. The same construction as the host's crates/wire/src/pc.rs and the
/// desktop's relay-core/pc.ts, all held to serve/docs/reference/fixtures/person-chat-vectors.json.
///
/// Encodings, fixed once: every identifier (contact id, msg id, user id, R, sender key) is the UTF-8 bytes of its
/// string. A recipient key R is the device's sign_pub as base64url without padding (43 chars) or `computer:<id>`; the
/// sender key is the sender device's sign_pub in the same base64url form. epk and box_pub are raw 32 bytes inside
/// hashes. Every label starts `kith/pc`, distinct from the mailbox family in RelayCrypto.
///
/// The zero nonce is safe only because every key it is used with is single-use: K is fresh per message and each wrap
/// key derives from a fresh ephemeral per recipient per message. Never seal twice under one K or one e_i.
public enum PersonChatCrypto {
    /// The cloud's limit on recipient devices per message.
    public static let maxRecipients = 32

    /// Why a seal or open refused. The raw value is the name the vectors use.
    public enum Failure: String, Error, Sendable {
        case noRecipients, tooManyRecipients, duplicateRecipient
        case notInDirectory, badRhash, badSignature, notRecipient, zeroSharedSecret, badWrap, badCiphertext
    }

    public struct Recipient: Equatable, Sendable {
        /// Device sign_pub (base64url) or `computer:<id>`.
        public var r: String
        public var epk: Data
        public var wrap: Data
        public init(r: String, epk: Data, wrap: Data) { (self.r, self.epk, self.wrap) = (r, epk, wrap) }
    }

    public struct Envelope: Equatable, Sendable {
        public var contactId: String
        public var epoch: UInt64
        public var msgId: String
        /// The sender device's sign_pub, base64url.
        public var senderKey: String
        public var ct: Data
        public var sig: Data
        public var rhash: Data
        /// Sorted by `r`.
        public var recipients: [Recipient]
        public init(contactId: String, epoch: UInt64, msgId: String, senderKey: String, ct: Data, sig: Data, rhash: Data, recipients: [Recipient]) {
            (self.contactId, self.epoch, self.msgId, self.senderKey) = (contactId, epoch, msgId, senderKey)
            (self.ct, self.sig, self.rhash, self.recipients) = (ct, sig, rhash, recipients)
        }
    }

    /// Who to seal to: the recipient key and the device's raw X25519 key (its box_sig is checked by the caller).
    public struct SealTo: Sendable {
        public var r: String
        public var boxPub: Data
        public init(r: String, boxPub: Data) { (self.r, self.boxPub) = (r, boxPub) }
    }

    /// Fixed randomness, for the vectors only: K, and one ephemeral private key per recipient in the order given.
    public struct Entropy: Sendable {
        public var key: Data
        public var ephemerals: [Data]
        public init(key: Data, ephemerals: [Data]) { (self.key, self.ephemerals) = (key, ephemerals) }
    }

    // MARK: pieces the cloud recomputes

    static func utf8(_ s: String) -> Data { Data(s.utf8) }

    /// The content AEAD's aad.
    public static func aad(contactId: String, epoch: UInt64, msgId: String) -> Data {
        utf8("kith/pc-msg/v1") + utf8(contactId) + RelayCrypto.u64be(epoch) + utf8(msgId)
    }

    private static func utf8Less(_ a: String, _ b: String) -> Bool { a.utf8.lexicographicallyPrecedes(b.utf8) }

    /// SHA-256 over the recipients sorted by R (UTF-8 byte order): R ‖ epk ‖ wrap for each.
    public static func rhash(_ recipients: [Recipient]) -> Data {
        let sorted = recipients.sorted { utf8Less($0.r, $1.r) }
        return RelayCrypto.sha256(sorted.reduce(Data()) { $0 + utf8($1.r) + $1.epk + $1.wrap })
    }

    /// What the sender signs and the cloud verifies at send.
    public static func sigInput(contactId: String, epoch: UInt64, msgId: String, senderKey: String, rhash: Data, ct: Data) -> Data {
        utf8("kith/pc/v1") + utf8(contactId) + RelayCrypto.u64be(epoch) + utf8(msgId) + utf8(senderKey) + rhash + RelayCrypto.sha256(ct)
    }

    /// What a device's sign key signs to vouch for its box key.
    public static func boxSigInput(userId: String, boxPub: Data) -> Data { utf8("kith/pc-box/v1") + utf8(userId) + boxPub }

    public static func makeBoxSig(signing: Curve25519.Signing.PrivateKey, userId: String, boxPub: Data) throws -> Data {
        try signing.signature(for: boxSigInput(userId: userId, boxPub: boxPub))
    }

    public static func verifyBoxSig(signPub: Data, userId: String, boxPub: Data, sig: Data) -> Bool {
        guard let key = try? Curve25519.Signing.PublicKey(rawRepresentation: signPub) else { return false }
        return key.isValidSignature(sig, for: boxSigInput(userId: userId, boxPub: boxPub))
    }

    // MARK: key schedule

    private static let zeroNonce = try! ChaChaPoly.Nonce(data: Data(count: 12))

    private static func wrapKey(msgId: String, r: String, epk: Data, sharedSecret ss: Data) -> SymmetricKey {
        HKDF<SHA256>.deriveKey(inputKeyMaterial: SymmetricKey(data: ss), salt: utf8("kith/pc-wrap/v1") + utf8(msgId) + utf8(r) + epk,
                               info: utf8("kith/pc-wrap-key/v1"), outputByteCount: 32)
    }

    private static func wrapAAD(msgId: String, r: String) -> Data { utf8(msgId) + utf8(r) }

    private static func encrypt(_ key: SymmetricKey, aad: Data, _ plaintext: Data) throws -> Data {
        let box = try ChaChaPoly.seal(plaintext, using: key, nonce: zeroNonce, authenticating: aad)
        return box.ciphertext + box.tag
    }

    private static func decrypt(_ key: SymmetricKey, aad: Data, _ ct: Data) -> Data? {
        guard ct.count >= 16 else { return nil }
        let body = ct.prefix(ct.count - 16), tag = ct.suffix(16)
        guard let box = try? ChaChaPoly.SealedBox(nonce: zeroNonce, ciphertext: body, tag: tag) else { return nil }
        return try? ChaChaPoly.open(box, using: key, authenticating: aad)
    }

    /// X25519 that refuses the all-zero result (a low-order peer key).
    private static func agree(_ priv: Curve25519.KeyAgreement.PrivateKey, _ pub: Data) throws -> Data {
        guard pub.count == 32, let key = try? Curve25519.KeyAgreement.PublicKey(rawRepresentation: pub),
              let ss = try? priv.sharedSecretFromKeyAgreement(with: key).withUnsafeBytes({ Data($0) }),
              ss.contains(where: { $0 != 0 })
        else { throw Failure.zeroSharedSecret }
        return ss
    }

    // MARK: seal

    /// Seals `plaintext` to every recipient; `sign` signs with the sender device's key (`senderPublicKey` is its
    /// 32 raw bytes). Pass `entropy` only in the vectors.
    public static func seal(plaintext: Data, contactId: String, epoch: UInt64, msgId: String, to: [SealTo],
                            senderPublicKey: Data, sign: (Data) throws -> Data, entropy: Entropy? = nil) throws -> Envelope {
        guard !to.isEmpty else { throw Failure.noRecipients }
        guard to.count <= maxRecipients else { throw Failure.tooManyRecipients }
        guard Set(to.map(\.r)).count == to.count else { throw Failure.duplicateRecipient }
        let kBytes = entropy?.key ?? Data.random(32)
        let ephemerals = try entropy?.ephemerals.map { try Curve25519.KeyAgreement.PrivateKey(rawRepresentation: $0) }
            ?? to.map { _ in Curve25519.KeyAgreement.PrivateKey() }
        let ct = try encrypt(SymmetricKey(data: kBytes), aad: aad(contactId: contactId, epoch: epoch, msgId: msgId), plaintext)
        var recipients: [Recipient] = []
        for (t, e) in zip(to, ephemerals) {
            let epk = e.publicKey.rawRepresentation
            let key = wrapKey(msgId: msgId, r: t.r, epk: epk, sharedSecret: try agree(e, t.boxPub))
            recipients.append(Recipient(r: t.r, epk: epk, wrap: try encrypt(key, aad: wrapAAD(msgId: msgId, r: t.r), kBytes)))
        }
        recipients.sort { utf8Less($0.r, $1.r) }
        let hash = rhash(recipients)
        let senderKey = senderPublicKey.base64URL
        let sig = try sign(sigInput(contactId: contactId, epoch: epoch, msgId: msgId, senderKey: senderKey, rhash: hash, ct: ct))
        return Envelope(contactId: contactId, epoch: epoch, msgId: msgId, senderKey: senderKey, ct: ct, sig: sig, rhash: hash, recipients: recipients)
    }

    // MARK: open

    /// Checks directory, rhash and sig, then unwraps K and opens the body. `directory` is the sender account's
    /// device sign_pub keys (base64url) from the contact's directory; `r` is the key this device was sealed under.
    public static func open(_ env: Envelope, as r: String, boxSecret: Curve25519.KeyAgreement.PrivateKey, directory: [String]) throws -> Data {
        guard directory.contains(env.senderKey) else { throw Failure.notInDirectory }
        guard rhash(env.recipients) == env.rhash else { throw Failure.badRhash }
        guard let raw = Data(base64URL: env.senderKey), raw.count == 32, let senderPub = try? Curve25519.Signing.PublicKey(rawRepresentation: raw),
              senderPub.isValidSignature(env.sig, for: sigInput(contactId: env.contactId, epoch: env.epoch, msgId: env.msgId,
                                                               senderKey: env.senderKey, rhash: env.rhash, ct: env.ct))
        else { throw Failure.badSignature }
        guard let mine = env.recipients.first(where: { $0.r == r }) else { throw Failure.notRecipient }
        let key = wrapKey(msgId: env.msgId, r: mine.r, epk: mine.epk, sharedSecret: try agree(boxSecret, mine.epk))
        guard let k = decrypt(key, aad: wrapAAD(msgId: env.msgId, r: mine.r), mine.wrap) else { throw Failure.badWrap }
        guard let body = decrypt(SymmetricKey(data: k), aad: aad(contactId: env.contactId, epoch: env.epoch, msgId: env.msgId), env.ct)
        else { throw Failure.badCiphertext }
        return body
    }
}
