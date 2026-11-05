//! Writing secrets back to the OS store at a rollback (what a module's `restore_secrets` calls). The rules:
//!
//! - Idempotent: an item the store already holds with the same value is not written again, so a resumed rollback or a second
//!   one changes nothing.
//! - Never deletes: [`OsStore`] has no delete. Records in the secrets envelope stay where they are, and a longer old split
//!   leaves its extra chunks behind (harmless: the marker says how many belong).
//! - Fails before anything is written: every item is checked first (sizes, names). The framework runs `restore_secrets`
//!   before `export_back`, so a refusal here leaves the files tombstoned and the record `rolling-back`.
//! - Size limits. The Windows Credential Manager refuses a blob over 2560 bytes (`CRED_MAX_CREDENTIAL_BLOB_SIZE`); the
//!   Keychain and the Secret Service have no limit worth handling. With a `limit`, a bigger value is SPLIT: chunks of at most
//!   `limit` bytes (cut on a character boundary) are stored as `<name>#0`, `<name>#1` ..., then the item `<name>` itself is
//!   written LAST holding `kith-split:1:<chunks>:<first 16 hex of the SHA-256 of the whole value>`, so a crash half-way leaves
//!   the old value readable, never a marker over missing chunks. [`read_back`] reassembles and checks the digest. More than
//!   [`MAX_CHUNKS`] chunks, or a value that itself starts with the marker text, is REFUSED (nothing written).

use std::collections::BTreeMap;

use sha2::{Digest as _, Sha256};
use zeroize::Zeroizing;

use crate::error::{Error, Result};

/// The most chunks one value may take (about 160 KiB at the Windows limit).
pub(crate) const MAX_CHUNKS: usize = 64;
/// The Windows Credential Manager blob limit, in bytes.
pub(crate) const WINDOWS_BLOB_LIMIT: usize = 2560;
const MARKER: &str = "kith-split:1:";

/// The OS secret store, as far as a rollback needs it: read and write by item name. No delete, on purpose.
pub(crate) trait OsStore {
    fn get(&self, name: &str) -> Result<Option<Zeroizing<String>>>;
    fn put(&mut self, name: &str, value: &str) -> Result<()>;
}

fn digest16(value: &str) -> String {
    Sha256::digest(value.as_bytes())
        .iter()
        .take(8)
        .fold(String::new(), |mut s, b| {
            use std::fmt::Write as _;
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// The chunks of `value`, each at most `limit` bytes, cut on character boundaries.
fn chunks(value: &str, limit: usize) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = value;
    while !rest.is_empty() {
        let mut end = limit.min(rest.len());
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        let (head, tail) = rest.split_at(end);
        out.push(head);
        rest = tail;
    }
    out
}

/// The item as the store holds it: a plain value, or a split one reassembled. `None` when absent or when a split value's
/// chunks are missing or fail their digest (so the caller writes it again).
pub(crate) fn read_back(store: &dyn OsStore, name: &str) -> Result<Option<Zeroizing<String>>> {
    let Some(head) = store.get(name)? else {
        return Ok(None);
    };
    let Some(spec) = head.strip_prefix(MARKER) else {
        return Ok(Some(head));
    };
    let Some((n, sum)) = spec.split_once(':') else {
        return Ok(None);
    };
    let Ok(n) = n.parse::<usize>() else {
        return Ok(None);
    };
    let mut whole = Zeroizing::new(String::new());
    for i in 0..n {
        match store.get(&format!("{name}#{i}"))? {
            Some(c) => whole.push_str(&c),
            None => return Ok(None),
        }
    }
    Ok((digest16(&whole) == sum).then_some(whole))
}

/// Writes every `(name, value)` back. `limit`: the store's blob limit in bytes (`Some(WINDOWS_BLOB_LIMIT)` on Windows).
/// Returns one line per item written and one for the count left alone.
pub(crate) fn restore_all(
    store: &mut dyn OsStore,
    items: &BTreeMap<String, String>,
    limit: Option<usize>,
) -> Result<Vec<String>> {
    // Everything that can refuse, before the first write.
    for (name, value) in items {
        if name.is_empty()
            || (name.contains('#')
                && name
                    .rsplit('#')
                    .next()
                    .is_some_and(|t| t.parse::<usize>().is_ok()))
        {
            return Err(Error::Refused(format!(
                "secrets: {name:?} cannot be an item name (empty, or it ends in #<number>, which names a chunk)"
            )));
        }
        if value.starts_with(MARKER) {
            return Err(Error::Refused(format!(
                "secrets: the value of {name} starts with the split marker; nothing was written back"
            )));
        }
        if let Some(l) = limit
            && (l == 0 || value.len().div_ceil(l) > MAX_CHUNKS)
        {
            return Err(Error::Refused(format!(
                "secrets: the value of {name} is {} bytes, more than {MAX_CHUNKS} chunks of {l}; nothing was written back",
                value.len()
            )));
        }
    }
    let (mut wrote, mut same) = (Vec::new(), 0usize);
    for (name, value) in items {
        if read_back(store, name)?.is_some_and(|v| v.as_str() == value.as_str()) {
            same += 1;
            continue;
        }
        match limit.filter(|l| value.len() > *l) {
            None => store.put(name, value)?,
            Some(l) => {
                let parts = chunks(value, l);
                for (i, c) in parts.iter().enumerate() {
                    store.put(&format!("{name}#{i}"), c)?;
                }
                // The marker last: until it lands, the old value is the one a reader sees.
                store.put(
                    name,
                    &format!("{MARKER}{}:{}", parts.len(), digest16(value)),
                )?;
            }
        }
        wrote.push(format!("secret written back: {name}"));
    }
    wrote.push(format!("secrets: {same} already in the store"));
    Ok(wrote)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    #[derive(Default)]
    struct Mem {
        items: BTreeMap<String, String>,
        limit: usize,
        puts: usize,
    }

    impl OsStore for Mem {
        fn get(&self, name: &str) -> Result<Option<Zeroizing<String>>> {
            Ok(self.items.get(name).cloned().map(Zeroizing::new))
        }
        fn put(&mut self, name: &str, value: &str) -> Result<()> {
            if self.limit > 0 && value.len() > self.limit {
                return Err(Error::Io("blob too large".into()));
            }
            self.puts += 1;
            self.items.insert(name.into(), value.into());
            Ok(())
        }
    }

    fn items(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn a_small_value_is_written_once_and_a_repeat_changes_nothing() {
        let mut s = Mem::default();
        let it = items(&[("a", "1"), ("b", "2")]);
        restore_all(&mut s, &it, None).unwrap();
        assert_eq!(s.puts, 2);
        let out = restore_all(&mut s, &it, None).unwrap();
        assert_eq!(s.puts, 2, "idempotent");
        assert!(out.last().unwrap().contains("2 already"));
    }

    #[test]
    fn a_big_value_is_split_under_the_limit_and_reads_back_whole() {
        // The store takes 40 bytes (the marker is about 31), chunks are cut at 10.
        let mut s = Mem {
            limit: 40,
            ..Mem::default()
        };
        // Multi-byte characters: the cut must not land inside one.
        let big = "é".repeat(23) + "end";
        restore_all(&mut s, &items(&[("k", &big)]), Some(10)).unwrap();
        assert!(s.items["k"].starts_with("kith-split:1:"));
        assert!(s.items.keys().any(|k| k == "k#0"));
        assert!(
            s.items
                .iter()
                .filter(|(k, _)| k.contains('#'))
                .all(|(_, v)| v.len() <= 10),
            "chunks are at most the limit"
        );
        assert_eq!(
            read_back(&s, "k").unwrap().as_ref().map(|v| v.as_str()),
            Some(big.as_str())
        );
        let before = s.puts;
        restore_all(&mut s, &items(&[("k", &big)]), Some(10)).unwrap();
        assert_eq!(
            s.puts, before,
            "a split value that reads back whole is left alone"
        );
    }

    #[test]
    fn a_missing_chunk_is_written_again_and_old_records_are_never_deleted() {
        let mut s = Mem::default();
        let big = "x".repeat(25);
        restore_all(&mut s, &items(&[("k", &big), ("other", "keep")]), Some(10)).unwrap();
        s.items.remove("k#1");
        assert_eq!(read_back(&s, "k").unwrap(), None);
        restore_all(&mut s, &items(&[("k", &big)]), Some(10)).unwrap();
        assert_eq!(
            read_back(&s, "k").unwrap().as_ref().map(|v| v.as_str()),
            Some(big.as_str())
        );
        assert_eq!(
            s.items["other"], "keep",
            "an item outside the call is untouched"
        );
    }

    #[test]
    fn too_big_or_ambiguous_values_are_refused_before_any_write() {
        let mut s = Mem::default();
        let huge = "x".repeat(10 * (MAX_CHUNKS + 1));
        let e = restore_all(&mut s, &items(&[("a", "1"), ("z", &huge)]), Some(10)).unwrap_err();
        assert!(e.to_string().contains("chunks"), "{e}");
        assert_eq!(s.puts, 0, "the small item was not written either");
        let e = restore_all(&mut s, &items(&[("m", "kith-split:1:9:x")]), None).unwrap_err();
        assert!(e.to_string().contains("marker"), "{e}");
        assert!(restore_all(&mut s, &items(&[("k#3", "v")]), None).is_err());
        assert_eq!(s.puts, 0);
    }
}
