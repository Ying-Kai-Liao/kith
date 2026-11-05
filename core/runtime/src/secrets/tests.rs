#![allow(clippy::unwrap_used)]

use std::collections::HashSet;

use rusqlite::Connection;

use super::keystore::MemoryKeyStore;
use super::{Envelope, KeyStore};
use crate::db::{MIGRATIONS, apply};

fn db() -> Connection {
    let mut c = Connection::open_in_memory().expect("memory db");
    apply(&mut c, MIGRATIONS, None).expect("migrate");
    c
}

fn envelope(store: &MemoryKeyStore) -> Envelope {
    Envelope::with_store(Box::new(store.clone()))
}

fn count(c: &Connection) -> i64 {
    c.query_row("SELECT COUNT(*) FROM secret_records", [], |r| r.get(0))
        .expect("count")
}

#[test]
fn round_trip_and_replace() {
    let c = db();
    let store = MemoryKeyStore::default();
    let mut e = envelope(&store);
    assert!(e.get(&c, "a").expect("get before").is_none());
    assert!(store.peek().is_none(), "a read never mints a key");
    e.put(&c, "a", b"one").expect("put");
    assert_eq!(&e.get(&c, "a").expect("get").expect("there")[..], b"one");
    e.put(&c, "a", b"two").expect("replace");
    assert_eq!(&e.get(&c, "a").expect("get").expect("there")[..], b"two");
    assert_eq!(count(&c), 1, "two puts of one name are one record");
    e.put(&c, "b", b"").expect("empty value");
    assert_eq!(Envelope::list(&c).expect("list"), ["a", "b"]);
    assert!(Envelope::delete(&c, "a").expect("delete"));
    assert!(!Envelope::delete(&c, "a").expect("delete again"));
    assert!(e.get(&c, "a").expect("gone").is_none());
    // A new Envelope on the same store (a daemon restart) reads what was written.
    let mut again = envelope(&store);
    assert_eq!(&again.get(&c, "b").expect("get").expect("there")[..], b"");
}

#[test]
fn nothing_plaintext_in_the_table() {
    let c = db();
    let store = MemoryKeyStore::default();
    let mut e = envelope(&store);
    e.put(&c, "svc/token", b"hunter2-hunter2").expect("put");
    let (ct, nonce): (Vec<u8>, Vec<u8>) = c
        .query_row("SELECT ciphertext, nonce FROM secret_records", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .expect("row");
    assert_eq!(nonce.len(), 24);
    assert_eq!(ct.len(), "hunter2-hunter2".len() + 16, "tag only");
    assert!(!ct.windows(7).any(|w| w == b"hunter2"));
}

#[test]
fn a_record_renamed_in_the_table_does_not_open() {
    let c = db();
    let store = MemoryKeyStore::default();
    let mut e = envelope(&store);
    e.put(&c, "low", b"secret").expect("put");
    c.execute("UPDATE secret_records SET name = 'high'", [])
        .expect("rename");
    let err = e.get(&c, "high").expect_err("wrong associated data");
    assert!(err.to_string().contains("did not open"), "{err}");
}

#[test]
fn tampered_ciphertext_does_not_open() {
    let c = db();
    let store = MemoryKeyStore::default();
    let mut e = envelope(&store);
    e.put(&c, "a", b"secret").expect("put");
    let mut ct: Vec<u8> = c
        .query_row("SELECT ciphertext FROM secret_records", [], |r| r.get(0))
        .expect("ct");
    ct[0] ^= 1;
    c.execute("UPDATE secret_records SET ciphertext = ?1", [ct])
        .expect("tamper");
    assert!(e.get(&c, "a").is_err());
    // Likewise a flipped nonce byte.
    e.put(&c, "a", b"secret").expect("put again");
    let mut n: Vec<u8> = c
        .query_row("SELECT nonce FROM secret_records", [], |r| r.get(0))
        .expect("nonce");
    n[23] ^= 1;
    c.execute("UPDATE secret_records SET nonce = ?1", [n])
        .expect("tamper nonce");
    assert!(e.get(&c, "a").is_err());
}

#[test]
fn an_unreadable_key_store_refuses_and_changes_nothing() {
    let c = db();
    let store = MemoryKeyStore::default();
    envelope(&store).put(&c, "a", b"x").expect("put");
    let before = store.peek();
    store.set_unreadable(true);
    let mut e = envelope(&store);
    assert!(e.get(&c, "a").is_err());
    assert!(e.put(&c, "b", b"y").is_err());
    store.set_unreadable(false);
    assert_eq!(store.peek(), before);
    assert_eq!(count(&c), 1);

    // An empty table with an unreadable store also refuses a put: "could not read" is never "no key".
    let c2 = db();
    let locked = MemoryKeyStore::default();
    locked.set_unreadable(true);
    assert!(envelope(&locked).put(&c2, "a", b"x").is_err());
    locked.set_unreadable(false);
    assert!(locked.peek().is_none(), "no key was minted");
}

#[test]
fn records_without_a_key_never_mint_a_new_one() {
    let c = db();
    let store = MemoryKeyStore::default();
    envelope(&store).put(&c, "a", b"x").expect("put");
    store.forget();
    let mut e = envelope(&store);
    for r in [
        e.put(&c, "b", b"y"),
        e.get(&c, "a").map(|_| ()),
        e.get(&c, "nope").map(|_| ()),
    ] {
        let err = r.expect_err("refuses");
        assert!(err.to_string().contains("missing"), "{err}");
    }
    assert!(store.peek().is_none(), "no key was made over the records");
    assert_eq!(count(&c), 1, "the record is untouched");
    // delete and list need no key.
    assert_eq!(Envelope::list(&c).expect("list"), ["a"]);
}

#[test]
fn a_fresh_key_only_when_there_are_no_records() {
    let c = db();
    let store = MemoryKeyStore::default();
    envelope(&store).put(&c, "a", b"x").expect("mints");
    assert_eq!(store.peek().expect("key").len(), 32);
    // Emptying the table makes a fresh key legitimate again.
    store.forget();
    assert!(Envelope::delete(&c, "a").expect("delete"));
    envelope(&store).put(&c, "a", b"y").expect("mints again");
    assert!(store.peek().is_some());
}

#[test]
fn a_key_from_another_machine_refuses_clearly() {
    let c = db();
    let mine = MemoryKeyStore::default();
    envelope(&mine).put(&c, "a", b"x").expect("put");
    let theirs = MemoryKeyStore::default();
    let other = db();
    envelope(&theirs)
        .put(&other, "z", b"z")
        .expect("their record");

    // My database with their key.
    let mut e = envelope(&theirs);
    let err = e.get(&c, "a").expect_err("different key");
    assert!(err.to_string().contains("different master key"), "{err}");
    // A put does not mix two keys in one table, and leaves the record alone.
    let err = e.put(&c, "b", b"y").expect_err("mixed keys");
    assert!(err.to_string().contains("different master key"), "{err}");
    assert_eq!(count(&c), 1);
    assert_eq!(
        &envelope(&mine).get(&c, "a").expect("get").expect("there")[..],
        b"x"
    );
}

#[test]
fn a_malformed_stored_key_refuses() {
    let c = db();
    let store = MemoryKeyStore::default();
    store.add(&[1, 2, 3]).expect("set");
    let err = envelope(&store).put(&c, "a", b"x").expect_err("short key");
    assert!(err.to_string().contains("wrong length"), "{err}");
    assert_eq!(store.peek(), Some(vec![1, 2, 3]), "left untouched");
}

#[test]
fn nonces_do_not_repeat() {
    let c = db();
    let store = MemoryKeyStore::default();
    let mut e = envelope(&store);
    let mut seen = HashSet::new();
    for _ in 0..2000 {
        e.put(&c, "a", b"same").expect("put");
        let n: Vec<u8> = c
            .query_row("SELECT nonce FROM secret_records", [], |r| r.get(0))
            .expect("nonce");
        assert!(seen.insert(n), "a nonce was used twice");
    }
}

#[test]
fn bad_names_are_refused() {
    let c = db();
    let store = MemoryKeyStore::default();
    let mut e = envelope(&store);
    assert!(e.put(&c, "", b"x").is_err());
    assert!(e.put(&c, &"n".repeat(257), b"x").is_err());
    assert!(e.put(&c, "a\nb", b"x").is_err());
    assert!(store.peek().is_none());
}

#[test]
fn the_real_migration_is_version_20_in_its_lane() {
    let m = MIGRATIONS.iter().find(|m| m.name == "secret_records");
    assert_eq!(m.expect("step").version, 20);
}

/// The real OS stores. A Keychain can prompt and a runner may have none, so these run only when asked for:
/// `KITH_TEST_OS_KEYSTORE=1`, on a machine where an unlock prompt is acceptable.
fn os_tests_enabled() -> bool {
    std::env::var_os("KITH_TEST_OS_KEYSTORE").is_some_and(|v| v == "1")
}

/// `replaces`: Linux's `secret-tool store` overwrites; macOS adds only and keeps the first item.
fn exercise(store: &dyn KeyStore, replaces: bool) {
    assert!(store.get().expect("get empty").is_none());
    let key: Vec<u8> = (0u8..32).collect();
    store.add(&key).expect("set");
    assert_eq!(&store.get().expect("get").expect("there")[..], &key[..]);
    let other: Vec<u8> = (32u8..64).collect();
    store.add(&other).expect("replace");
    let kept = if replaces { &other } else { &key };
    assert_eq!(&store.get().expect("get").expect("there")[..], &kept[..]);
}

#[test]
#[cfg(target_os = "macos")]
fn os_store_macos_security_cli() {
    if !os_tests_enabled() {
        return;
    }
    let s = super::keystore::SecurityCli::new(
        &format!("dev.kith.test.{}", std::process::id()),
        "master-key",
    );
    s.remove();
    exercise(&s, false);
    s.remove();
}

#[test]
#[cfg(target_os = "linux")]
fn os_store_linux_secret_tool() {
    if !os_tests_enabled() {
        return;
    }
    let s = super::keystore::SecretTool::new(
        &format!("dev.kith.test.{}", std::process::id()),
        "master-key",
    );
    s.remove();
    exercise(&s, true);
    s.remove();
}

#[test]
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn os_store_is_unsupported_here() {
    assert!(super::keystore::os_key_store().is_err());
    let _ = (
        super::keystore::SecurityCli::new("a", "b"),
        super::keystore::SecretTool::new("a", "b"),
    );
    let _ = os_tests_enabled();
    let _: fn(&dyn KeyStore, bool) = exercise;
}

fn slot(store: &MemoryKeyStore) -> std::sync::Mutex<Option<Envelope>> {
    std::sync::Mutex::new(Some(envelope(store)))
}

#[test]
fn the_rows_open_put_and_get() {
    use serde_json::json;
    let c = db();
    let store = MemoryKeyStore::default();
    let s = slot(&store);
    let call = |m: &str, p: serde_json::Value| super::call(&c, &s, m, &p);
    assert_eq!(
        call("secrets.get", json!({"name": "a"})).expect("get"),
        json!({"found": false})
    );
    assert!(store.peek().is_none(), "a read never mints a key");
    call("secrets.open", json!({})).expect("open");
    assert!(
        store.peek().is_some(),
        "open mints the key on an empty table"
    );
    call("secrets.put", json!({"name": "a", "value": "one"})).expect("put");
    call("secrets.put", json!({"name": "a", "value": "two"})).expect("replace");
    call("secrets.put", json!({"name": "e", "value": ""})).expect("empty");
    assert_eq!(
        call("secrets.get", json!({"name": "a"})).expect("get"),
        json!({"found": true, "value": "two"})
    );
    assert_eq!(
        call("secrets.get", json!({"name": "e"})).expect("get"),
        json!({"found": true, "value": ""})
    );
    assert_eq!(count(&c), 2);
    assert_eq!(
        call("secrets.has", json!({"name": "a"})).expect("has"),
        json!({"has": true})
    );
    assert_eq!(
        call("secrets.has", json!({"name": "zz"})).expect("has"),
        json!({"has": false})
    );
    assert!(matches!(
        call("secrets.put", json!({"name": "a"})),
        Err(crate::Error::BadParams(_))
    ));
    assert_eq!(
        call("secrets.delete", json!({"name": "a"})).expect("delete"),
        json!({"deleted": true})
    );
    assert_eq!(
        call("secrets.delete", json!({"name": "a"})).expect("again"),
        json!({"deleted": false})
    );
    assert_eq!(
        call("secrets.has", json!({"name": "a"})).expect("has"),
        json!({"has": false})
    );
    assert!(matches!(
        call("secrets.list", json!({})),
        Err(crate::Error::NotServed(_))
    ));
}

#[test]
fn the_rows_refuse_without_the_key_and_keep_the_records() {
    use serde_json::json;
    let c = db();
    let store = MemoryKeyStore::default();
    let s = slot(&store);
    super::call(&c, &s, "secrets.put", &json!({"name": "a", "value": "one"})).expect("put");
    store.forget();
    let fresh = slot(&store);
    for (m, p) in [
        ("secrets.get", json!({"name": "a"})),
        ("secrets.open", json!({})),
        ("secrets.put", json!({"name": "b", "value": "two"})),
    ] {
        let err = super::call(&c, &fresh, m, &p).expect_err(m);
        assert!(
            err.to_string().contains("master key is missing"),
            "{m}: {err}"
        );
    }
    assert!(store.peek().is_none(), "no new key over existing records");
    assert_eq!(count(&c), 1);
}

#[test]
fn has_and_delete_never_touch_the_key_store() {
    use serde_json::json;
    let c = db();
    let store = MemoryKeyStore::default();
    let s = slot(&store);
    super::call(&c, &s, "secrets.put", &json!({"name": "a", "value": "one"})).expect("put");
    // A locked key store (and then no key at all): the rows that need no key still answer, and mint nothing.
    store.set_unreadable(true);
    let fresh = slot(&store);
    let call = |m: &str, p: serde_json::Value| super::call(&c, &fresh, m, &p);
    assert_eq!(
        call("secrets.has", json!({"name": "a"})).expect("has"),
        json!({"has": true})
    );
    assert_eq!(
        call("secrets.has", json!({"name": "b"})).expect("has"),
        json!({"has": false})
    );
    assert!(
        call("secrets.get", json!({"name": "a"})).is_err(),
        "get does need the key"
    );
    assert_eq!(
        call("secrets.delete", json!({"name": "a"})).expect("delete"),
        json!({"deleted": true})
    );
    store.set_unreadable(false);
    store.forget();
    assert_eq!(
        call("secrets.has", json!({"name": "a"})).expect("has"),
        json!({"has": false})
    );
    assert!(store.peek().is_none(), "no row minted a key");
}

#[test]
fn every_secrets_row_is_the_owners_except_get_which_is_the_hosts_alone() {
    use serde_json::json;
    let rows: Vec<_> = kith_api::METHODS
        .iter()
        .filter(|m| m.name.starts_with("secrets."))
        .collect();
    assert!(rows.len() >= 5, "the secrets rows are in the table");
    for row in rows {
        // `get` is the only row that hands a value back, and the owner token (a file any process of the user reads)
        // is not enough for it: the host's in-memory proof is (M2-10c).
        let allowed = if row.name == "secrets.get" {
            kith_api::Principal::Host
        } else {
            kith_api::Principal::Owner
        };
        for who in kith_api::Principal::ALL {
            let answer = crate::call::check(who, row.name, &json!({"name": "a", "value": "v"}));
            if who == allowed {
                // The params here do not fit every row; the principal is only never turned away for who it is.
                assert!(
                    !matches!(answer, Err(crate::Error::NotAllowed(_))),
                    "{}: {answer:?}",
                    row.name
                );
            } else {
                assert!(
                    matches!(answer, Err(crate::Error::NotAllowed(_))),
                    "{} for {}: {answer:?}",
                    row.name,
                    who.name()
                );
            }
        }
    }
}

/// Two agents, three people's worth of profiles: the fixture `kith-auth`'s `secrets(auth, svc, profile)` and `granted`
/// read (profile -> VAR -> source, grants agent[@person] -> service -> profile). Values sit in the envelope under the
/// `keychain:` reference itself, as the import seals them.
fn launch_fixture(c: &Connection, e: &mut Envelope) {
    let sql = |q: &str, a: &[&str]| {
        c.execute(q, rusqlite::params_from_iter(a)).expect("seed");
    };
    let profile = |p: &str, person: Option<&str>, lent: Option<&str>| {
        c.execute(
            "INSERT INTO secret_profiles (profile, person, lent) VALUES (?1, ?2, ?3)",
            rusqlite::params![p, person, lent],
        )
        .expect("profile");
    };
    let var = |p: &str, v: &str, s: &str| {
        sql(
            "INSERT INTO secret_vars (profile, var, source) VALUES (?1, ?2, ?3)",
            &[p, v, s],
        );
    };
    let grant = |a: &str, s: &str, p: &str, seq: i64| {
        c.execute(
            "INSERT INTO secret_grants (agent, service, profile, seq) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![a, s, p, seq],
        )
        .expect("grant");
    };
    profile("github/work", Some("kai"), None);
    profile("github/other", Some("kai"), None);
    profile("github/sam", Some("sam"), None);
    profile("github/lent", Some("kai"), Some("[\"sam\"]"));
    profile("mail/me", Some("kai"), None);
    profile("browser/coder", None, None); // no variables, by design
    profile("browser/kai", Some("kai"), None); // the owner's own browser, no variables
    var("github/work", "GITHUB_TOKEN", "keychain:kith-github#work");
    var("github/work", "UNUSED", "keychain:kith-github#unused");
    var("github/other", "GITHUB_TOKEN", "keychain:kith-github#other");
    var("github/sam", "GITHUB_TOKEN", "keychain:kith-github#sam");
    var("github/lent", "GITHUB_TOKEN", "keychain:kith-github#work");
    var("mail/me", "MAIL_USER", "keychain:kith-mail#user");
    var("mail/me", "MAIL_PASS", "keychain:kith-mail#pass");
    var("mail/me", "MAIL_HOST", "env:MAIL_HOST");
    grant("coder", "github", "github/work", 0);
    grant("coder", "github", "github/other", 1);
    grant("coder", "mail", "mail/me", 0);
    grant("coder", "browser", "browser/coder", 0);
    grant("guestbot", "browser", "browser/kai", 0);
    grant("ghostly", "browser", "browser/missing", 0); // a grant whose profile row is gone
    grant("coder@sam", "github", "github/sam", 0);
    grant("house", "github", "github/other", 0);
    for (name, value) in [
        ("keychain:kith-github#work", "ghp_work"),
        ("keychain:kith-github#unused", "never"),
        ("keychain:kith-github#other", "ghp_other"),
        ("keychain:kith-github#sam", "ghp_sam"),
        ("keychain:kith-mail#user", "me@example.com"),
        ("keychain:kith-mail#pass", "pw"),
    ] {
        e.put(c, name, value.as_bytes()).expect("put");
    }
}

#[allow(clippy::needless_pass_by_value)] // call sites build the json! inline
fn keys(
    c: &Connection,
    s: &std::sync::Mutex<Option<Envelope>>,
    p: serde_json::Value,
) -> crate::error::Result<serde_json::Value> {
    // `person` is required; the tests that do not care whom a run is for pass "" (no person check).
    let mut p = p;
    if let Some(o) = p.as_object_mut() {
        o.entry("person").or_insert(serde_json::json!(""));
    }
    super::call(c, s, "secrets.serviceKeys", &p)
}

#[test]
fn service_keys_returns_only_the_declared_variables_of_the_granted_profile() {
    use serde_json::json;
    let c = db();
    let store = MemoryKeyStore::default();
    let s = slot(&store);
    launch_fixture(&c, s.lock().unwrap().as_mut().unwrap());
    // The profile also holds UNUSED; the service does not declare it, so it stays home. The first listed profile wins.
    assert_eq!(
        keys(
            &c,
            &s,
            json!({"agent": "coder", "service": "github", "vars": ["GITHUB_TOKEN"]})
        )
        .expect("github"),
        json!({"agent": "coder", "service": "github", "profile": "github/work",
               "keys": [{"var": "GITHUB_TOKEN", "value": "ghp_work"}], "unresolved": []})
    );
    // `profile` picks another of the list; one that is not granted is refused.
    let other = keys(&c, &s, json!({"agent": "coder", "service": "github", "vars": ["GITHUB_TOKEN"], "profile": "github/other"})).expect("pick");
    assert!(other.to_string().contains("ghp_other") && !other.to_string().contains("ghp_work"));
    assert!(keys(&c, &s, json!({"agent": "coder", "service": "github", "vars": ["GITHUB_TOKEN"], "profile": "github/sam"})).is_err());
    // Another agent's key is never in an answer.
    let house = keys(
        &c,
        &s,
        json!({"agent": "house", "service": "github", "vars": ["GITHUB_TOKEN"]}),
    )
    .expect("house")
    .to_string();
    assert!(house.contains("ghp_other") && !house.contains("ghp_work") && !house.contains("pw"));
    // A non-envelope source comes back for the caller to resolve; the envelope ones are filled.
    let mail = keys(
        &c,
        &s,
        json!({"agent": "coder", "service": "mail", "vars": ["MAIL_USER", "MAIL_HOST"]}),
    )
    .expect("mail");
    assert_eq!(
        mail["keys"],
        json!([{"var": "MAIL_USER", "value": "me@example.com"}])
    );
    assert_eq!(
        mail["unresolved"],
        json!([{"var": "MAIL_HOST", "source": "env:MAIL_HOST"}])
    );
    // A service that declares no secrets reads no profile: a browser profile with no variables is fine.
    assert_eq!(
        keys(
            &c,
            &s,
            json!({"agent": "coder", "service": "browser", "vars": []})
        )
        .expect("browser")["keys"],
        json!([])
    );
    // No vars still applies the grant rule, and names the profile for the caller's audit.
    let e = keys(
        &c,
        &s,
        json!({"agent": "nobody", "service": "x", "vars": []}),
    )
    .expect_err("no grant even with no vars")
    .to_string();
    assert!(e.contains("kith-auth grant nobody x"), "{e}");
    assert_eq!(
        keys(
            &c,
            &s,
            json!({"agent": "coder", "service": "browser", "vars": []})
        )
        .expect("browser")["profile"],
        json!("browser/coder")
    );
}

#[test]
fn service_keys_applies_the_person_rules_and_refuses_what_kith_auth_refuses() {
    use serde_json::json;
    let c = db();
    let store = MemoryKeyStore::default();
    let s = slot(&store);
    launch_fixture(&c, s.lock().unwrap().as_mut().unwrap());
    let g = |extra: serde_json::Value| {
        let mut p = json!({"agent": "coder", "service": "github", "vars": ["GITHUB_TOKEN"]});
        p.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        keys(&c, &s, p)
    };
    // The person's own grant (coder@sam) comes first.
    assert!(
        g(json!({"person": "sam"}))
            .expect("sam")
            .to_string()
            .contains("ghp_sam")
    );
    // A person with no grant of their own falls to the agent's, which is kai's: refused unless lent.
    let e = g(json!({"person": "jo"}))
        .expect_err("not theirs")
        .to_string();
    assert!(e.contains("github/work") && e.contains("kai's"), "{e}");
    assert!(!e.contains("ghp_"));
    c.execute(
        "UPDATE secret_profiles SET lent = '[\"jo\"]' WHERE profile = 'github/work'",
        [],
    )
    .expect("lend");
    assert!(
        g(json!({"person": "jo"}))
            .expect("lent")
            .to_string()
            .contains("ghp_work")
    );
    // The owner's own run is checked like anyone's: their own profiles pass, another person's unlent one is refused.
    assert!(g(json!({"person": "kai"})).is_ok());
    let e = keys(
        &c,
        &s,
        json!({"agent": "house", "service": "github", "vars": ["GITHUB_TOKEN"], "person": "sam"}),
    )
    .expect_err("kai's")
    .to_string();
    assert!(e.contains("kai's"), "{e}");
    c.execute(
        "UPDATE secret_grants SET agent = 'coder@kai' WHERE profile = 'github/sam'",
        [],
    )
    .expect("regrant");
    let e = g(json!({"person": "kai"}))
        .expect_err("sam's profile on kai's run")
        .to_string();
    assert!(e.contains("sam's") && !e.contains("ghp_"), "{e}");
    // "" is the only no-check value.
    assert!(g(json!({})).is_ok());
    // No vars does not skip the grant or the person rule: a guest asking for the owner's browser is refused, the owner
    // is not, and a grant whose profile row is gone says "no profile" (not "not set").
    let browser_of = |agent: &str, person: &str| {
        keys(
            &c,
            &s,
            json!({"agent": agent, "service": "browser", "vars": [], "person": person}),
        )
    };
    let e = browser_of("guestbot", "jo").expect_err("guest").to_string();
    assert!(e.contains("kai's") && e.contains("jo"), "{e}");
    assert_eq!(
        browser_of("guestbot", "kai").expect("owner")["profile"],
        json!("browser/kai")
    );
    let e = browser_of("ghostly", "kai")
        .expect_err("no row")
        .to_string();
    assert!(
        e.contains("no profile 'browser/missing'") && !e.contains("not set"),
        "{e}"
    );
    let e = g(json!({"vars": ["NOPE"]})).expect_err("var").to_string();
    assert!(e.contains("not set") && !e.contains("no profile"), "{e}");
    // `person` itself is required through the call.
    assert!(
        super::call(
            &c,
            &s,
            "secrets.serviceKeys",
            &json!({"agent": "coder", "service": "github", "vars": []})
        )
        .is_err()
    );
    // No grant: the hint, not a value.
    let e = keys(
        &c,
        &s,
        json!({"agent": "ghost", "service": "github", "vars": ["GITHUB_TOKEN"]}),
    )
    .expect_err("no grant")
    .to_string();
    assert!(e.contains("kith-auth grant ghost github"), "{e}");
    // A declared variable with no row, a record gone, and an empty value each refuse, naming the variable.
    let e = g(json!({"vars": ["GITHUB_TOKEN", "NOPE"]}))
        .expect_err("no row")
        .to_string();
    assert!(e.contains("NOPE") && !e.contains("ghp_"), "{e}");
    Envelope::delete(&c, "keychain:kith-mail#pass").expect("delete");
    let e = keys(
        &c,
        &s,
        json!({"agent": "coder", "service": "mail", "vars": ["MAIL_USER", "MAIL_PASS"]}),
    )
    .expect_err("gone")
    .to_string();
    assert!(
        e.contains("MAIL_PASS") && !e.contains("me@example.com"),
        "{e}"
    );
    s.lock()
        .unwrap()
        .as_mut()
        .unwrap()
        .put(&c, "keychain:kith-mail#pass", b"")
        .expect("empty");
    assert!(
        keys(
            &c,
            &s,
            json!({"agent": "coder", "service": "mail", "vars": ["MAIL_PASS"]})
        )
        .is_err()
    );
    // A key a card (put) rewrote shows up on the next call.
    super::call(
        &c,
        &s,
        "secrets.put",
        &json!({"name": "keychain:kith-github#work", "value": "ghp_new"}),
    )
    .expect("put");
    assert!(g(json!({})).expect("again").to_string().contains("ghp_new"));
}

#[test]
fn service_keys_refuses_without_the_master_key() {
    use serde_json::json;
    let c = db();
    let store = MemoryKeyStore::default();
    let s = slot(&store);
    launch_fixture(&c, s.lock().unwrap().as_mut().unwrap());
    let lost = slot(&MemoryKeyStore::default());
    let e = keys(
        &c,
        &lost,
        json!({"agent": "coder", "service": "github", "vars": ["GITHUB_TOKEN"]}),
    )
    .expect_err("locked")
    .to_string();
    assert!(e.contains("master key"), "{e}");
}

#[test]
fn service_keys_on_a_database_imported_before_grants_were_kept_says_to_import_again() {
    use serde_json::json;
    let c = db();
    let store = MemoryKeyStore::default();
    let s = slot(&store);
    launch_fixture(&c, s.lock().unwrap().as_mut().unwrap());
    c.execute_batch(
        "DELETE FROM secret_grants; INSERT INTO secret_stale VALUES ('imported before secret_grants');",
    )
    .expect("stale");
    let e = keys(
        &c,
        &s,
        json!({"agent": "coder", "service": "github", "vars": ["GITHUB_TOKEN"], "person": "sam"}),
    )
    .expect_err("stale")
    .to_string();
    assert!(e.contains("import again"), "{e}");
}
