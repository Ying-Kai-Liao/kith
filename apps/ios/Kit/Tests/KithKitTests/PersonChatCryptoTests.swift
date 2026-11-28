import CryptoKit
import Foundation
import Testing
@testable import KithKit

/// serve/docs/reference/fixtures/person-chat-vectors.json: the file the host's crates/wire/src/pc.rs and the desktop's
/// relay-core/pc.test.ts are held to as well.
private let pc: [String: Any] = {
    let url = URL(filePath: #filePath).deletingLastPathComponent().appending(path: "../../../../../serve/docs/reference/fixtures/person-chat-vectors.json").standardized
    return ((try? Data(contentsOf: url)).flatMap { try? JSONSerialization.jsonObject(with: $0) } as? [String: Any]) ?? [:]
}()

private func dict(_ v: Any?) -> [String: Any] { v as? [String: Any] ?? [:] }
private func list(_ v: Any?) -> [[String: Any]] { v as? [[String: Any]] ?? [] }
private func b64(_ v: Any?) -> Data { Data(base64URL: v as? String ?? "") ?? Data() }
private func hex(_ d: Data) -> String { d.map { String(format: "%02x", $0) }.joined() }

private func envelope(_ j: [String: Any]) -> PersonChatCrypto.Envelope {
    .init(contactId: j["contact_id"] as? String ?? "", epoch: UInt64(j["epoch"] as? Int ?? 0), msgId: j["msg_id"] as? String ?? "",
          senderKey: j["sender_key"] as? String ?? "", ct: b64(j["ct"]), sig: b64(j["sig"]), rhash: b64(j["rhash"]),
          recipients: list(j["recipients"]).map { .init(r: $0["r"] as? String ?? "", epk: b64($0["epk"]), wrap: b64($0["wrap"])) })
}

@Test func personChatVectorFileLoads() {
    #expect(!pc.isEmpty)
}

@Test func personChatSealReproducesTheVectorBytes() throws {
    let s = dict(pc["seal"])
    let signing = try Curve25519.Signing.PrivateKey(rawRepresentation: b64(dict(pc["keys"])["sender_sign_seed"]))
    let to = list(s["recipients"]).map { PersonChatCrypto.SealTo(r: $0["r"] as? String ?? "", boxPub: b64($0["box_pub"])) }
    let entropy = PersonChatCrypto.Entropy(key: b64(s["content_key"]), ephemerals: (s["ephemerals"] as? [String] ?? []).map { b64($0) })
    let env = try PersonChatCrypto.seal(plaintext: Data((s["plaintext"] as? String ?? "").utf8), contactId: s["contact_id"] as? String ?? "",
                                        epoch: UInt64(s["epoch"] as? Int ?? 0), msgId: s["msg_id"] as? String ?? "", to: to,
                                        senderPublicKey: signing.publicKey.rawRepresentation, sign: { try signing.signature(for: $0) }, entropy: entropy)
    let want = envelope(dict(s["expect"]))
    // CryptoKit's Ed25519 signatures are randomized: everything but the signature is compared by bytes, the signature by verifying.
    var got = env
    got.sig = want.sig
    #expect(got == want)
    let input = PersonChatCrypto.sigInput(contactId: env.contactId, epoch: env.epoch, msgId: env.msgId, senderKey: env.senderKey, rhash: env.rhash, ct: env.ct)
    #expect(hex(input) == s["sig_input_hex"] as? String)
    #expect(hex(PersonChatCrypto.aad(contactId: env.contactId, epoch: env.epoch, msgId: env.msgId)) == s["aad_hex"] as? String)
    #expect(signing.publicKey.isValidSignature(env.sig, for: input))
    #expect(signing.publicKey.isValidSignature(want.sig, for: input))
}

@Test func personChatOpenCasesFromTheVectors() throws {
    let plaintext = Data((dict(pc["seal"])["plaintext"] as? String ?? "").utf8)
    let cases = list(pc["open"])
    #expect(cases.count >= 10)
    for c in cases {
        let name = c["name"] as? String ?? ""
        let secret = try Curve25519.KeyAgreement.PrivateKey(rawRepresentation: b64(c["box_secret"]))
        let run = { try PersonChatCrypto.open(envelope(dict(c["envelope"])), as: c["r"] as? String ?? "", boxSecret: secret, directory: c["directory"] as? [String] ?? []) }
        let expect = c["expect"] as? String ?? ""
        if expect == "ok" {
            #expect(try run() == plaintext, Comment(rawValue: name))
        } else {
            do {
                _ = try run()
                Issue.record("\(name): opened")
            } catch let e as PersonChatCrypto.Failure {
                #expect(e.rawValue == expect, Comment(rawValue: name))
            }
        }
    }
}

@Test func personChatBoxSigCasesFromTheVectors() throws {
    for c in list(dict(pc["box_sig"])["cases"]) {
        let ok = PersonChatCrypto.verifyBoxSig(signPub: b64(c["sign_pub"]), userId: c["user_id"] as? String ?? "", boxPub: b64(c["box_pub"]), sig: b64(c["sig"]))
        #expect(ok == (c["valid"] as? Bool), Comment(rawValue: c["name"] as? String ?? ""))
    }
    // make then verify with a fresh key (signatures are randomized, so no byte compare).
    let key = Curve25519.Signing.PrivateKey()
    let box = Curve25519.KeyAgreement.PrivateKey().publicKey.rawRepresentation
    let sig = try PersonChatCrypto.makeBoxSig(signing: key, userId: "user_x", boxPub: box)
    #expect(PersonChatCrypto.verifyBoxSig(signPub: key.publicKey.rawRepresentation, userId: "user_x", boxPub: box, sig: sig))
}

@Test func personChatSealRefusesBadRecipientListsAndRoundTrips() throws {
    let signing = Curve25519.Signing.PrivateKey()
    let bob = Curve25519.KeyAgreement.PrivateKey()
    let one = PersonChatCrypto.SealTo(r: "R", boxPub: bob.publicKey.rawRepresentation)
    func go(_ to: [PersonChatCrypto.SealTo]) throws -> PersonChatCrypto.Envelope {
        try PersonChatCrypto.seal(plaintext: Data("hi".utf8), contactId: "ct_x", epoch: 1, msgId: "m1", to: to,
                                  senderPublicKey: signing.publicKey.rawRepresentation, sign: { try signing.signature(for: $0) })
    }
    #expect(throws: PersonChatCrypto.Failure.noRecipients) { try go([]) }
    #expect(throws: PersonChatCrypto.Failure.duplicateRecipient) { try go([one, one]) }
    #expect(throws: PersonChatCrypto.Failure.tooManyRecipients) {
        try go((0...PersonChatCrypto.maxRecipients).map { PersonChatCrypto.SealTo(r: "r\($0)", boxPub: one.boxPub) })
    }
    let a = try go([one]), b = try go([one])
    #expect(a.recipients[0].epk != b.recipients[0].epk)
    #expect(a.ct != b.ct)
    #expect(try PersonChatCrypto.open(a, as: "R", boxSecret: bob, directory: [a.senderKey]) == Data("hi".utf8))
}
