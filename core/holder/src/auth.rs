//! The hello's mutual proof. Each side sends a nonce; each answers with HMAC-SHA256 over both nonces, its role and
//! the generation, keyed by the secret. The secret never crosses the wire, a replayed answer fails against a fresh
//! nonce, and the holder proves it knows the key before the core says anything it cares about.

use hmac::{Hmac, Mac};
use sha2::Sha256;

pub const CORE: &str = "core";
pub const HOLDER: &str = "holder";

/// The proof `role` gives for this exchange, as lowercase hex.
pub fn proof(
    key: &[u8],
    role: &str,
    core_nonce: &str,
    holder_nonce: &str,
    generation: u64,
) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("hmac takes any key length");
    for part in [
        role.as_bytes(),
        core_nonce.as_bytes(),
        holder_nonce.as_bytes(),
    ] {
        // A separator that cannot occur in hex keeps ("ab","c") and ("a","bc") apart.
        mac.update(part);
        mac.update(&[0]);
    }
    mac.update(&generation.to_le_bytes());
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Compares without stopping at the first difference.
pub fn same(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_proof_binds_role_nonces_generation_and_key() {
        let p = proof(b"k", CORE, "aa", "bb", 1);
        assert_eq!(p, proof(b"k", CORE, "aa", "bb", 1));
        for other in [
            proof(b"k2", CORE, "aa", "bb", 1),
            proof(b"k", HOLDER, "aa", "bb", 1),
            proof(b"k", CORE, "ab", "bb", 1),
            proof(b"k", CORE, "aa", "bc", 1),
            proof(b"k", CORE, "aa", "bb", 2),
            proof(b"k", CORE, "aab", "b", 1),
        ] {
            assert!(!same(p.as_bytes(), other.as_bytes()));
        }
    }
}
