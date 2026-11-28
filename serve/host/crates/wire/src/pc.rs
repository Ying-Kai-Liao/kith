//! Person-chat wire crypto (designs/2026-11-24-app-to-design/phase4a-build.md §3). One random content key `K` seals the
//! body once; only `K` is wrapped per recipient device (X25519 + HKDF-SHA256 + ChaCha20-Poly1305), and the sender signs
//! what the cloud and every recipient can recompute. The same construction as apps/desktop `relay-core/pc.ts` and
//! `KithKit` `PersonChatCrypto.swift`, all held to `docs/reference/fixtures/person-chat-vectors.json`.
//!
//! Encodings, fixed once: every identifier (contact id, msg id, user id, `R`, sender key) is the UTF-8 bytes of its
//! string. A recipient key `R` is the device's `sign_pub` as base64url without padding (43 chars) or `computer:<id>`;
//! the sender key is the sender device's `sign_pub` in the same base64url form. `epk` and `box_pub` are raw 32 bytes
//! inside hashes. Every label starts `kith/pc`, distinct from the mailbox family in `crypto`.
//!
//! The zero nonce is safe only because every key it is used with is single-use: `K` is fresh per message and each
//! wrap key derives from a fresh ephemeral per recipient per message. Never seal twice under one `K` or one `e_i`.

use std::fmt;

use chacha20poly1305::ChaCha20Poly1305;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use ed25519_dalek::SigningKey;
use hkdf::Hkdf;
use sha2::Sha256;
use x25519_dalek::StaticSecret;

use crate::crypto::{b64, random, sha256, sign, unb64_n, verify, x25519, x25519_pub};

/// The cloud's limit on recipient devices per message.
pub const MAX_RECIPIENTS: usize = 32;

/// Why a seal or open refused. `code()` is the name the vectors use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PcError {
    NoRecipients,
    TooManyRecipients,
    DuplicateRecipient,
    NotInDirectory,
    BadRhash,
    BadSignature,
    NotRecipient,
    ZeroSharedSecret,
    BadWrap,
    BadCiphertext,
}

impl PcError {
    pub fn code(self) -> &'static str {
        match self {
            PcError::NoRecipients => "noRecipients",
            PcError::TooManyRecipients => "tooManyRecipients",
            PcError::DuplicateRecipient => "duplicateRecipient",
            PcError::NotInDirectory => "notInDirectory",
            PcError::BadRhash => "badRhash",
            PcError::BadSignature => "badSignature",
            PcError::NotRecipient => "notRecipient",
            PcError::ZeroSharedSecret => "zeroSharedSecret",
            PcError::BadWrap => "badWrap",
            PcError::BadCiphertext => "badCiphertext",
        }
    }
}

impl fmt::Display for PcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for PcError {}

/// One recipient's entry in an envelope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recipient {
    /// Device `sign_pub` (base64url) or `computer:<id>`.
    pub r: String,
    pub epk: [u8; 32],
    pub wrap: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Envelope {
    pub contact_id: String,
    pub epoch: u64,
    pub msg_id: String,
    /// The sender device's `sign_pub`, base64url.
    pub sender_key: String,
    pub ct: Vec<u8>,
    pub sig: [u8; 64],
    pub rhash: [u8; 32],
    /// Sorted by `r`.
    pub recipients: Vec<Recipient>,
}

/// Who to seal to: the recipient key and the device's raw X25519 key (its `box_sig` is checked by the caller).
#[derive(Clone, Copy, Debug)]
pub struct SealTo<'a> {
    pub r: &'a str,
    pub box_pub: [u8; 32],
}

/// Fixed randomness, for the vectors only: `K`, and one ephemeral private key per recipient in the order given.
pub struct Entropy {
    pub key: [u8; 32],
    pub ephemerals: Vec<[u8; 32]>,
}

// MARK: pieces the cloud recomputes

/// The content AEAD's aad.
pub fn aad_of(contact_id: &str, epoch: u64, msg_id: &str) -> Vec<u8> {
    [b"kith/pc-msg/v1".as_slice(), contact_id.as_bytes(), &epoch.to_be_bytes(), msg_id.as_bytes()].concat()
}

/// SHA-256 over the recipients sorted by `R` (UTF-8 byte order): `R ‖ epk ‖ wrap` for each.
pub fn rhash_of(recipients: &[Recipient]) -> [u8; 32] {
    let mut sorted: Vec<&Recipient> = recipients.iter().collect();
    sorted.sort_by(|a, b| a.r.as_bytes().cmp(b.r.as_bytes()));
    let parts: Vec<&[u8]> = sorted.iter().flat_map(|x| [x.r.as_bytes(), x.epk.as_slice(), x.wrap.as_slice()]).collect();
    sha256(&parts)
}

/// What the sender signs and the cloud verifies at send.
pub fn sig_input(contact_id: &str, epoch: u64, msg_id: &str, sender_key: &str, rhash: &[u8; 32], ct: &[u8]) -> Vec<u8> {
    [
        b"kith/pc/v1".as_slice(),
        contact_id.as_bytes(),
        &epoch.to_be_bytes(),
        msg_id.as_bytes(),
        sender_key.as_bytes(),
        rhash,
        &sha256(&[ct]),
    ]
    .concat()
}

/// What a device's sign key signs to vouch for its box key.
pub fn box_sig_input(user_id: &str, box_pub: &[u8; 32]) -> Vec<u8> {
    [b"kith/pc-box/v1".as_slice(), user_id.as_bytes(), box_pub].concat()
}

pub fn make_box_sig(sign_key: &SigningKey, user_id: &str, box_pub: &[u8; 32]) -> [u8; 64] {
    sign(sign_key, &box_sig_input(user_id, box_pub))
}

pub fn verify_box_sig(sign_pub: &[u8; 32], user_id: &str, box_pub: &[u8; 32], sig: &[u8; 64]) -> bool {
    verify(sign_pub, &box_sig_input(user_id, box_pub), sig).is_ok()
}

// MARK: key schedule

fn wrap_key(msg_id: &str, r: &str, epk: &[u8; 32], ss: &[u8; 32]) -> [u8; 32] {
    let salt = [b"kith/pc-wrap/v1".as_slice(), msg_id.as_bytes(), r.as_bytes(), epk].concat();
    let mut out = [0u8; 32];
    Hkdf::<Sha256>::new(Some(&salt), ss)
        .expand(b"kith/pc-wrap-key/v1", &mut out)
        .expect("32 bytes is a valid HKDF-SHA256 output length");
    out
}

fn wrap_aad(msg_id: &str, r: &str) -> Vec<u8> {
    [msg_id.as_bytes(), r.as_bytes()].concat()
}

fn encrypt(key: &[u8; 32], aad: &[u8], msg: &[u8]) -> Vec<u8> {
    ChaCha20Poly1305::new(key.into())
        .encrypt(&[0u8; 12].into(), Payload { msg, aad })
        .expect("ChaCha20-Poly1305 only fails past 256 GiB")
}

fn decrypt(key: &[u8], aad: &[u8], ct: &[u8]) -> Option<Vec<u8>> {
    let key: &[u8; 32] = key.try_into().ok()?;
    ChaCha20Poly1305::new(key.into()).decrypt(&[0u8; 12].into(), Payload { msg: ct, aad }).ok()
}

// MARK: seal

/// Seals `plaintext` to every recipient. `entropy` is `None` outside the vectors.
pub fn seal(
    sender: &SigningKey,
    contact_id: &str,
    epoch: u64,
    msg_id: &str,
    plaintext: &[u8],
    to: &[SealTo<'_>],
    entropy: Option<Entropy>,
) -> Result<Envelope, PcError> {
    if to.is_empty() {
        return Err(PcError::NoRecipients);
    }
    if to.len() > MAX_RECIPIENTS {
        return Err(PcError::TooManyRecipients);
    }
    let mut seen: Vec<&str> = to.iter().map(|t| t.r).collect();
    seen.sort_unstable();
    if seen.windows(2).any(|w| w[0] == w[1]) {
        return Err(PcError::DuplicateRecipient);
    }
    let (k, ephemerals) = match entropy {
        Some(e) => (e.key, e.ephemerals),
        None => (random::<32>(), to.iter().map(|_| random::<32>()).collect()),
    };
    let ct = encrypt(&k, &aad_of(contact_id, epoch, msg_id), plaintext);
    let mut recipients = Vec::with_capacity(to.len());
    for (t, e) in to.iter().zip(ephemerals) {
        let e = StaticSecret::from(e);
        let epk = x25519_pub(&e);
        let ss = x25519(&e, &t.box_pub).map_err(|_| PcError::ZeroSharedSecret)?;
        let key = wrap_key(msg_id, t.r, &epk, &ss);
        recipients.push(Recipient { r: t.r.to_string(), epk, wrap: encrypt(&key, &wrap_aad(msg_id, t.r), &k) });
    }
    recipients.sort_by(|a, b| a.r.as_bytes().cmp(b.r.as_bytes()));
    let rhash = rhash_of(&recipients);
    let sender_key = b64(sender.verifying_key().as_bytes());
    let sig = sign(sender, &sig_input(contact_id, epoch, msg_id, &sender_key, &rhash, &ct));
    Ok(Envelope { contact_id: contact_id.into(), epoch, msg_id: msg_id.into(), sender_key, ct, sig, rhash, recipients })
}

// MARK: open

/// Checks directory, rhash and sig, then unwraps `K` and opens the body. `directory` is the sender account's device
/// `sign_pub` keys (base64url) from the contact's directory; `r` is the key this device was sealed under.
pub fn open(r: &str, box_secret: &StaticSecret, env: &Envelope, directory: &[String]) -> Result<Vec<u8>, PcError> {
    if !directory.contains(&env.sender_key) {
        return Err(PcError::NotInDirectory);
    }
    if rhash_of(&env.recipients) != env.rhash {
        return Err(PcError::BadRhash);
    }
    let input = sig_input(&env.contact_id, env.epoch, &env.msg_id, &env.sender_key, &env.rhash, &env.ct);
    let sender_pub: [u8; 32] = unb64_n(&env.sender_key).map_err(|_| PcError::BadSignature)?;
    verify(&sender_pub, &input, &env.sig).map_err(|_| PcError::BadSignature)?;
    let mine = env.recipients.iter().find(|x| x.r == r).ok_or(PcError::NotRecipient)?;
    let ss = x25519(box_secret, &mine.epk).map_err(|_| PcError::ZeroSharedSecret)?;
    let key = wrap_key(&env.msg_id, &mine.r, &mine.epk, &ss);
    let k = decrypt(&key, &wrap_aad(&env.msg_id, &mine.r), &mine.wrap).ok_or(PcError::BadWrap)?;
    decrypt(&k, &aad_of(&env.contact_id, env.epoch, &env.msg_id), &env.ct).ok_or(PcError::BadCiphertext)
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;
    use crate::crypto::unb64;

    fn vectors() -> Value {
        serde_json::from_str(include_str!("../../../../docs/reference/fixtures/person-chat-vectors.json")).unwrap()
    }

    fn s(v: &Value) -> &str {
        v.as_str().unwrap()
    }

    fn raw<const N: usize>(v: &Value) -> [u8; N] {
        unb64_n(s(v)).unwrap()
    }

    fn envelope_of(j: &Value) -> Envelope {
        Envelope {
            contact_id: s(&j["contact_id"]).into(),
            epoch: j["epoch"].as_u64().unwrap(),
            msg_id: s(&j["msg_id"]).into(),
            sender_key: s(&j["sender_key"]).into(),
            ct: unb64(s(&j["ct"])).unwrap(),
            sig: raw(&j["sig"]),
            rhash: raw(&j["rhash"]),
            recipients: j["recipients"]
                .as_array()
                .unwrap()
                .iter()
                .map(|x| Recipient { r: s(&x["r"]).into(), epk: raw(&x["epk"]), wrap: unb64(s(&x["wrap"])).unwrap() })
                .collect(),
        }
    }

    fn sender(v: &Value) -> SigningKey {
        SigningKey::from_bytes(&raw(&v["keys"]["sender_sign_seed"]))
    }

    fn entropy(v: &Value) -> Entropy {
        Entropy {
            key: raw(&v["seal"]["content_key"]),
            ephemerals: v["seal"]["ephemerals"].as_array().unwrap().iter().map(raw).collect(),
        }
    }

    fn recipients(v: &Value) -> Vec<(String, [u8; 32])> {
        v["seal"]["recipients"].as_array().unwrap().iter().map(|x| (s(&x["r"]).into(), raw(&x["box_pub"]))).collect()
    }

    fn hex(b: &[u8]) -> String {
        use std::fmt::Write as _;
        b.iter().fold(String::new(), |mut o, x| {
            write!(o, "{x:02x}").unwrap();
            o
        })
    }

    #[test]
    fn seal_reproduces_the_vector_bytes() {
        let v = vectors();
        let to = recipients(&v);
        let to: Vec<SealTo<'_>> = to.iter().map(|(r, box_pub)| SealTo { r, box_pub: *box_pub }).collect();
        let seal_v = &v["seal"];
        let env = seal(
            &sender(&v),
            s(&seal_v["contact_id"]),
            seal_v["epoch"].as_u64().unwrap(),
            s(&seal_v["msg_id"]),
            s(&seal_v["plaintext"]).as_bytes(),
            &to,
            Some(entropy(&v)),
        )
        .unwrap();
        assert_eq!(env, envelope_of(&seal_v["expect"]));
        assert_eq!(hex(&aad_of(&env.contact_id, env.epoch, &env.msg_id)), s(&seal_v["aad_hex"]));
        let input = sig_input(&env.contact_id, env.epoch, &env.msg_id, &env.sender_key, &env.rhash, &env.ct);
        assert_eq!(hex(&input), s(&seal_v["sig_input_hex"]));
    }

    #[test]
    fn open_cases_from_the_vectors() {
        let v = vectors();
        for c in v["open"].as_array().unwrap() {
            let dir: Vec<String> = c["directory"].as_array().unwrap().iter().map(|k| s(k).to_string()).collect();
            let secret = StaticSecret::from(raw::<32>(&c["box_secret"]));
            let got = open(s(&c["r"]), &secret, &envelope_of(&c["envelope"]), &dir);
            let name = s(&c["name"]);
            match s(&c["expect"]) {
                "ok" => assert_eq!(got.unwrap(), v["seal"]["plaintext"].as_str().unwrap().as_bytes(), "{name}"),
                code => assert_eq!(got.unwrap_err().code(), code, "{name}"),
            }
        }
    }

    #[test]
    fn box_sig_cases_from_the_vectors() {
        let v = vectors();
        for c in v["box_sig"]["cases"].as_array().unwrap() {
            let ok = verify_box_sig(&raw(&c["sign_pub"]), s(&c["user_id"]), &raw(&c["box_pub"]), &raw(&c["sig"]));
            assert_eq!(ok, c["valid"].as_bool().unwrap(), "{}", s(&c["name"]));
        }
        let a = &v["keys"]["device_a"];
        let key = SigningKey::from_bytes(&raw(&a["sign_seed"]));
        let good = &v["box_sig"]["cases"][0];
        assert_eq!(b64(&make_box_sig(&key, s(&good["user_id"]), &raw(&good["box_pub"]))), s(&good["sig"]));
    }

    #[test]
    fn seal_refuses_bad_recipient_lists() {
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let one = SealTo { r: "a", box_pub: x25519_pub(&StaticSecret::from([9u8; 32])) };
        let go = |to: &[SealTo<'_>]| seal(&key, "ct_x", 1, "m", b"hi", to, None).unwrap_err();
        assert_eq!(go(&[]), PcError::NoRecipients);
        assert_eq!(go(&[one, one]), PcError::DuplicateRecipient);
        let names: Vec<String> = (0..=MAX_RECIPIENTS).map(|i| format!("r{i}")).collect();
        let many: Vec<SealTo<'_>> = names.iter().map(|r| SealTo { r, box_pub: one.box_pub }).collect();
        assert_eq!(go(&many), PcError::TooManyRecipients);
    }

    #[test]
    fn a_fresh_seal_round_trips() {
        let key = SigningKey::from_bytes(&random::<32>());
        let secret = StaticSecret::from(random::<32>());
        let to = [SealTo { r: "R", box_pub: x25519_pub(&secret) }];
        let dir = [b64(key.verifying_key().as_bytes())];
        let a = seal(&key, "ct_x", 1, "m1", b"hi", &to, None).unwrap();
        let b = seal(&key, "ct_x", 1, "m1", b"hi", &to, None).unwrap();
        assert_ne!(a.recipients[0].epk, b.recipients[0].epk);
        assert_ne!(a.ct, b.ct);
        assert_eq!(open("R", &secret, &a, &dir).unwrap(), b"hi");
    }
}
