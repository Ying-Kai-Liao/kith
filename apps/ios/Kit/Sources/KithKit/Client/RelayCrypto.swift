import CryptoKit
import Foundation

// The relay protocol's cryptography (docs/reference/remote-relay.md §5, §6), checked field by field
// against docs/reference/fixtures/remote-relay-vectors.json (`kith/*` labels). Labels are ASCII without a trailing NUL.

extension Data {
    /// Strict base64url without padding (RFC 4648 §5).
    init?(base64URL string: String) {
        guard string.utf8.allSatisfy({ Data.urlAlphabet.contains($0) }), string.count % 4 != 1 else { return nil }
        var s = string.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        s += String(repeating: "=", count: (4 - s.count % 4) % 4)
        self.init(base64Encoded: s)
    }

    var base64URL: String {
        base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }

    static func random(_ count: Int) -> Data {
        var g = SystemRandomNumberGenerator()
        return Data((0..<count).map { _ in UInt8.random(in: .min ... .max, using: &g) })
    }

    private static let urlAlphabet = Set("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_".utf8)
}

public enum RelayCryptoError: Error, Equatable {
    case badKey
    case zeroSharedSecret
    case badSignature
    case decryptFailed
    case badCounter
    case tooLarge
    case malformed
}

/// Which family of domain-separation labels a pairing speaks. Only `kith/*` exists; a stored value
/// this build doesn't know reads as it.
public enum LabelFamily: Codable, Sendable, CaseIterable, RawRepresentable {
    case kith

    /// Newest first: the order a verifier tries them in.
    static let newestFirst: [LabelFamily] = [.kith]

    public var rawValue: String {
        switch self {
        case .kith: "kith"
        }
    }

    public init?(rawValue: String) {
        switch rawValue {
        case "kith": self = .kith
        default: return nil
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.singleValueContainer()
        try container.encode(rawValue)
    }

    public init(from decoder: any Decoder) throws {
        self = Self(rawValue: try decoder.singleValueContainer().decode(String.self)) ?? .kith
    }

    /// `<family>/<rest>`, e.g. `kith/hs1/v1`.
    func label(_ rest: String) -> String { "\(rawValue)/\(rest)" }
}

public enum RelayCrypto {
    /// Largest plaintext chunk per frame (§6.3).
    static let chunkSize = 256 * 1024
    /// Reassembly limit for host → device messages.
    static let maxInbound = 16 * 1024 * 1024

    static func sha256(_ parts: Data...) -> Data {
        var h = SHA256()
        for p in parts { h.update(data: p) }
        return Data(h.finalize())
    }

    static func label(_ s: String) -> Data { Data(s.utf8) }
    static func label(_ f: LabelFamily, _ rest: String) -> Data { label(f.label(rest)) }

    static func u64be(_ v: UInt64) -> Data { withUnsafeBytes(of: v.bigEndian) { Data($0) } }

    // MARK: identifiers

    public static func computerId(signKey: Data) -> ComputerID { computerIdRaw(signKey: signKey).base64URL }

    static func computerIdRaw(signKey: Data) -> Data { sha256(signKey).prefix(16) }

    /// The 16 raw bytes behind a computer ID, or nil when it isn't one.
    static func computerIdRaw(_ id: ComputerID) -> Data? {
        guard id.count == 22, let raw = Data(base64URL: id), raw.count == 16 else { return nil }
        return raw
    }

    /// `offerId` of a pairing code (§4.1).
    static func offerId(_ f: LabelFamily, code: Data) -> String { sha256(label(f, "offer/v1"), code).prefix(16).base64URL }

    // MARK: X25519 + HKDF

    static func sharedSecret(_ priv: Curve25519.KeyAgreement.PrivateKey, _ pub: Data) throws -> Data {
        guard pub.count == 32, let key = try? Curve25519.KeyAgreement.PublicKey(rawRepresentation: pub) else { throw RelayCryptoError.badKey }
        let ss = try priv.sharedSecretFromKeyAgreement(with: key).withUnsafeBytes { Data($0) }
        guard ss.contains(where: { $0 != 0 }) else { throw RelayCryptoError.zeroSharedSecret }
        return ss
    }

    static func extract(salt: Data, ikm: Data) -> Data {
        HKDF<SHA256>.extract(inputKeyMaterial: SymmetricKey(data: ikm), salt: salt).withUnsafeBytes { Data($0) }
    }

    static func expand(_ prk: Data, _ f: LabelFamily, _ info: String) -> SymmetricKey {
        HKDF<SHA256>.expand(pseudoRandomKey: prk, info: label(f, info), outputByteCount: 32)
    }

    // MARK: handshake (§6.2)

    static func hs1Input(_ f: LabelFamily, cid: Data, dk: Data, ekD: Data, n: Data) -> Data {
        label(f, "hs1/v1") + cid + dk + ekD + n
    }

    static func transcriptHash(_ f: LabelFamily, cid: Data, dk: Data, ekD: Data, n: Data, ekH: Data) -> Data {
        sha256(label(f, "hs2/v1"), cid, dk, ekD, n, ekH)
    }

    struct ChannelKeys {
        /// The labels this channel runs under; frames use them as AAD.
        var family: LabelFamily
        var prk: Data
        var d2h: SymmetricKey
        var h2d: SymmetricKey
    }

    static func channelKeys(_ f: LabelFamily, sharedSecret ss: Data, transcriptHash th: Data) -> ChannelKeys {
        let prk = extract(salt: th, ikm: ss)
        return ChannelKeys(family: f, prk: prk, d2h: expand(prk, f, "d2h/v1"), h2d: expand(prk, f, "h2d/v1"))
    }

    /// The device's half of one handshake: fresh ephemeral key and nonce each connection.
    struct Handshake {
        let family: LabelFamily
        let cid: Data
        let dk: Data
        let hostSignKey: Curve25519.Signing.PublicKey
        let ephemeral: Curve25519.KeyAgreement.PrivateKey
        let n: Data

        init(computer: Computer, deviceKey: Data,
             ephemeral: Curve25519.KeyAgreement.PrivateKey = .init(), n: Data = .random(32)) throws {
            guard let sk = Data(base64URL: computer.signKey), sk.count == 32,
                  let hostKey = try? Curve25519.Signing.PublicKey(rawRepresentation: sk),
                  computerId(signKey: sk) == computer.id else { throw RelayCryptoError.badKey }
            family = computer.labelFamily
            cid = computerIdRaw(signKey: sk)
            dk = deviceKey
            hostSignKey = hostKey
            self.ephemeral = ephemeral
            self.n = n
        }

        var ekD: Data { ephemeral.publicKey.rawRepresentation }
        var signInput: Data { hs1Input(family, cid: cid, dk: dk, ekD: ekD, n: n) }

        /// Checks `welcome.sig` against the pinned host key, then derives both directions' keys.
        func finish(ekH: Data, sig: Data) throws -> ChannelKeys {
            let th = transcriptHash(family, cid: cid, dk: dk, ekD: ekD, n: n, ekH: ekH)
            guard hostSignKey.isValidSignature(sig, for: th) else { throw RelayCryptoError.badSignature }
            return channelKeys(family, sharedSecret: try sharedSecret(ephemeral, ekH), transcriptHash: th)
        }
    }

    // MARK: frames (§6.3)

    static func frameNonce(_ c: UInt64) -> Data { Data(count: 4) + u64be(c) }
    static func frameAAD(_ f: LabelFamily, _ c: UInt64) -> Data { label(f, "frame/v1") + u64be(c) }

    static func sealFrame(_ f: LabelFamily, key: SymmetricKey, counter c: UInt64, final: Bool, chunk: Data) throws -> Data {
        let box = try ChaChaPoly.seal(Data([final ? 0 : 1]) + chunk, using: key,
                                      nonce: ChaChaPoly.Nonce(data: frameNonce(c)), authenticating: frameAAD(f, c))
        return box.ciphertext + box.tag
    }

    static func openFrame(_ f: LabelFamily, key: SymmetricKey, counter c: UInt64, d: Data) throws -> (final: Bool, chunk: Data) {
        guard d.count >= 17 else { throw RelayCryptoError.malformed }
        let box = try ChaChaPoly.SealedBox(nonce: ChaChaPoly.Nonce(data: frameNonce(c)), ciphertext: d.dropLast(16), tag: d.suffix(16))
        guard let plain = try? ChaChaPoly.open(box, using: key, authenticating: frameAAD(f, c)),
              let flag = plain.first, flag <= 1 else { throw RelayCryptoError.decryptFailed }
        return (flag == 0, plain.dropFirst())
    }

    /// One direction's sender: every inner message becomes ≤ 256 KiB chunks with consecutive counters.
    struct FrameSealer {
        let family: LabelFamily
        let key: SymmetricKey
        private(set) var counter: UInt64 = 0

        init(family: LabelFamily, key: SymmetricKey) {
            self.family = family
            self.key = key
        }

        /// Past 2^32 frames the channel must be replaced (`4011 rekey`).
        var exhausted: Bool { counter >= 1 << 32 }

        mutating func seal(_ message: Data) throws -> [(c: UInt64, d: Data)] {
            var out: [(UInt64, Data)] = []
            var rest = message[...]
            repeat {
                let chunk = rest.prefix(RelayCrypto.chunkSize)
                rest = rest.dropFirst(chunk.count)
                out.append((counter, try sealFrame(family, key: key, counter: counter, final: rest.isEmpty, chunk: Data(chunk))))
                counter += 1
            } while !rest.isEmpty
            return out
        }
    }

    /// One direction's receiver: demands the exact next counter and reassembles chunks.
    struct FrameOpener {
        let family: LabelFamily
        let key: SymmetricKey
        let limit: Int
        private(set) var counter: UInt64 = 0
        private var buffer = Data()

        init(family: LabelFamily, key: SymmetricKey, limit: Int = RelayCrypto.maxInbound) {
            self.family = family
            self.key = key
            self.limit = limit
        }

        /// A whole message once its last chunk arrived, else nil.
        mutating func open(c: UInt64, d: Data) throws -> Data? {
            guard c == counter else { throw RelayCryptoError.badCounter }
            let (final, chunk) = try openFrame(family, key: key, counter: c, d: d)
            counter += 1
            guard buffer.count + chunk.count <= limit else { throw RelayCryptoError.tooLarge }
            buffer += chunk
            guard final else { return nil }
            defer { buffer = Data() }
            return buffer
        }
    }

    // MARK: mailbox (§6.4)

    struct Mailbox {
        var epk: Data
        var ciphertext: Data
        var sig: Data
        var blob: Data { epk + sig + ciphertext }
    }

    /// Seals one inner message for an offline host. A fresh ephemeral key every time (MUST):
    /// the fixed nonce is only safe because the key is never reused.
    static func sealMailbox(_ plaintext: Data, family f: LabelFamily, hostBoxKey: Data, computerId: ComputerID, deviceKey dk: Data, clientNonce: String,
                            sign: (Data) throws -> Data,
                            ephemeral: Curve25519.KeyAgreement.PrivateKey = .init()) throws -> Mailbox {
        guard let cid = computerIdRaw(computerId) else { throw RelayCryptoError.badKey }
        let epk = ephemeral.publicKey.rawRepresentation
        let ss = try sharedSecret(ephemeral, hostBoxKey)
        let prk = extract(salt: label(f, "mbox/v1") + cid + dk + epk, ikm: ss)
        let box = try ChaChaPoly.seal(plaintext, using: expand(prk, f, "mbox-key/v1"),
                                      nonce: ChaChaPoly.Nonce(data: Data(count: 12)), authenticating: dk + Data(clientNonce.utf8))
        let ct = box.ciphertext + box.tag
        return Mailbox(epk: epk, ciphertext: ct, sig: try sign(label(f, "mbox/v1") + cid + epk + ct))
    }

    static func mailboxKey(_ f: LabelFamily, sharedSecret ss: Data, cid: Data, dk: Data, epk: Data) -> SymmetricKey {
        expand(extract(salt: label(f, "mbox/v1") + cid + dk + epk, ikm: ss), f, "mbox-key/v1")
    }

    // MARK: push (§6.7)

    static func pushKey(_ f: LabelFamily, sharedSecret ss: Data, cid: Data, pushKey: Data, epk: Data) -> SymmetricKey {
        expand(extract(salt: label(f, "push/v1") + cid + pushKey + epk, ikm: ss), f, "push-key/v1")
    }

    /// Opens `sealed = b64url(epk ‖ ct)` with the device's push key. The AEAD tag says which label family
    /// the host sealed it under, so both are tried: an update never strands a notification.
    static func openPush(sealed: String, computerId: ComputerID, pushPrivate: Curve25519.KeyAgreement.PrivateKey) throws -> Data {
        guard let raw = Data(base64URL: sealed), raw.count > 32 + 16, let cid = computerIdRaw(computerId) else { throw RelayCryptoError.malformed }
        let epk = raw.prefix(32)
        let ct = raw.dropFirst(32)
        let ss = try sharedSecret(pushPrivate, Data(epk))
        let box = try ChaChaPoly.SealedBox(nonce: ChaChaPoly.Nonce(data: Data(count: 12)), ciphertext: ct.dropLast(16), tag: ct.suffix(16))
        for f in LabelFamily.newestFirst {
            let key = pushKey(f, sharedSecret: ss, cid: cid, pushKey: pushPrivate.publicKey.rawRepresentation, epk: Data(epk))
            if let plain = try? ChaChaPoly.open(box, using: key, authenticating: cid) { return plain }
        }
        throw RelayCryptoError.decryptFailed
    }

    // MARK: SAS (§4.2 B)

    static func sasCommit(_ f: LabelFamily, deviceKey dk: Data, deviceNonce nD: Data) -> Data {
        sha256(label(f, "sascommit/v1"), dk, nD)
    }

    static func sasCode(_ f: LabelFamily, hostSignKey: Data, deviceKey dk: Data, deviceNonce nD: Data, hostNonce nH: Data) -> String {
        let h = sha256(label(f, "sas/v2"), hostSignKey, dk, nD, nH)
        let v = h.prefix(4).reduce(UInt32(0)) { $0 << 8 | UInt32($1) }
        let s = String(v % 1_000_000)
        return String(repeating: "0", count: 6 - s.count) + s
    }

    // MARK: Kith-Sig (§5)

    /// The `Kith-Sig` header name. The cloud still accepts the pre-rename header and signature prefix from older apps.
    static let signatureHeaderName = "Kith-Sig"

    static func requestSigInput(method: String, authority: String, pathAndQuery: String, ts: Int64, nonce: String, body: Data,
                                family: LabelFamily = .kith) -> Data {
        Data([
            "\(family.rawValue)-sig-v1", method.uppercased(), authority.lowercased(), pathAndQuery,
            String(ts), nonce, sha256(body).base64URL,
        ].joined(separator: "\n").utf8)
    }

    /// The `Host` header value of `url`: lowercased, with a non-default port.
    static func authority(of url: URL) -> String {
        let host = (url.host(percentEncoded: false) ?? "").lowercased()
        let defaultPort = ["https": 443, "wss": 443, "http": 80, "ws": 80][url.scheme?.lowercased() ?? ""]
        guard let port = url.port, port != defaultPort else { return host }
        return "\(host):\(port)"
    }

    /// Path and query exactly as sent on the request line.
    static func pathAndQuery(of url: URL) -> String {
        let path = url.path(percentEncoded: true)
        let query = url.query(percentEncoded: true).map { "?\($0)" } ?? ""
        return (path.isEmpty ? "/" : path) + query
    }
}
