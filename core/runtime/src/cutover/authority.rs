//! The `authority` module of the cutover framework (M2-18a): who may use what, moved from `auth.json` into core.db behind
//! the framework's `step` / `rollback` / `check` and the `cutover.mayServe` gate. The switch stays OFF: nothing here
//! flips by itself, and `KITH_CORE_AUTHORITY` (read once, in `mod.rs`) still has to be set AND pass the gate.
//!
//! | hook             | what it does here                                                                                    |
//! |------------------|------------------------------------------------------------------------------------------------------|
//! | `tombstones`     | the caller's `auth.json`. (The host vault's rows in serve.db are not tombstoned: the host moves them itself at its first unlock, M2-7.) |
//! | `import`         | `import::run_in` (M2-8, `kith kernel core-migrate --authority`): idempotent, a snapshot first, `--dry-run`, `--force`, the Keychain items sealed into the envelope |
//! | `same_answer`    | `import::same_answer`: per person owner, revoked (yes/no), `bot_of`, `was`; the `retired` ids; the order of people; then `check.rs`: `may`, `resolve`, `resolve_bot`, `cut_off` over every person x agent x via x once, at the caller's "now" (so a grant that has expired or not started yet counts as the answer it gives now). NOT compared: the contents of invites, presets, `owner_bots_budget`, a bot's caps, and when a revoke or a grant was written. None of them is read by the host guard today (`may`, `resolve`, `resolve_bot`, `cut_off`); the host reading one of them is the point at which it joins the compare |
//! | `fingerprint`    | serve.db and its WAL (size, mtime), the number of vault rows in it, and the digest of core.db's authority tables (so a write made only through the core changes it); the framework adds auth.json's inode, size and mtime |
//! | `digest`         | the first 16 hex of the SHA-256 of core.db's authority tables, as the marker stores it                |
//! | `export_back`    | core.db -> auth.json, the shape `kith-auth` reads: the tables from core.db over the pre-cutover copy's other keys (`profiles`, `grants`, `env`, a person's `handles` ...) |
//! | `restore_secrets`| the envelope's `keychain:` records -> the OS store, before `export_back`                              |
//!
//! # What the file cannot say once it is a tombstone
//! `export_back` needs the keys the core never held (`profiles`, `grants`, `env`, `once`, `agent_access`, a person's
//! `handles` / `root` / `kith` / `agents`). They come from the copy `tombstone::ensure` kept beside the file
//! (`auth.json.pre-cutover-<time>`, the newest); with none, those tables are written empty and the report says so.
//!
//! # Refused on purpose
//! - An `auth.json` that holds the owner AND revoked on one person (import and `same_answer` both say which person).
//! - A tombstone as the source (a second `step --force` must not read the note as an empty file and wipe the core).
//! - A secret that cannot go back: `restore_secrets` checks every item (name, size) before it writes any, and the
//!   framework runs it before `export_back`, so the file stays a tombstone and the record stays `rolling-back`.
//!
//! # Blocks the flip of the verbs (M2-18), not this module
//! `kith-auth` moves to the core's `people.*` rows later; until each of these is done, that verb must stay on Python:
//! - `people.addBot` / `people.removeBot`: the same transaction must clear the removed id's `agent@<id>` grants and its
//!   place in profiles' `with` (Python `forget`), else a bot handed over by remove-and-re-add gets its old logins back
//!   (test `forget_also_clears_grants_and_profiles`, ignored until then).
//! - `people.removePerson --unused` and the `retired` check: `handles`, `agent@<id>` grants and owned profiles must be
//!   checked in the same transaction (core.db has no `handles` column and no lane B tables yet).
//! - `kith-auth agent-access ... [--rm]` writes only auth.json: the import keeps the `agent_access` table and the rules
//!   read it (m0013), but no core method writes it, so the verb stays on Python until one does.
//!
//! These do not change what the gate (`may_serve`) serves, which is the READ side (`may`, `resolve`, the request guard);
//! they are why `export_back` takes `profiles` and `grants` from the old copy and not from the core.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use kith_kernel::secrets::Ctx;
use rusqlite::Connection;
use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};
use zeroize::{Zeroize as _, Zeroizing};

use super::secrets_back::{self, OsStore};
use super::{Env, Module, Opts, tombstone};
use crate::AuthorityOptions;
use crate::db::Core;
use crate::error::{Error, Result};
use crate::import::items::{Item, ItemSource, Keychain, OsEnvelope, SecretSink};
use crate::secrets::{Envelope, keystore};

/// The person keys the core holds (the import's list): `export_back` takes them from core.db and everything else a person
/// has from the old copy.
const PERSON_KEYS: &[&str] = &[
    "name", "lang", "tz", "owner", "bot_of", "revoked", "was", "provider",
];
const MEMBER_KEYS: &[&str] = &[
    "preset",
    "mode",
    "trusted",
    "deny",
    "model",
    "env",
    "helpers",
    "budget",
    "access",
    "jobs",
    "helper_once",
    "access_by",
    "access_since",
];
const PRESET_KEYS: &[&str] = &["mode", "trusted", "deny"];
/// Tables `export_back` writes even when no old copy has them (a file `kith-auth` can load).
const EMPTY_TABLES: &[&str] = &["profiles", "grants", "env", "once"];

/// The envelope's `keychain:` records, as text: what a rollback writes back to the OS store.
pub(crate) trait Vault {
    fn keychain_records(
        &mut self,
        conn: &Connection,
    ) -> Result<BTreeMap<String, Zeroizing<String>>>;
}

/// The real envelope (the OS key store holds its master key).
#[derive(Default)]
struct EnvelopeVault(Option<Envelope>);

impl Vault for EnvelopeVault {
    fn keychain_records(
        &mut self,
        conn: &Connection,
    ) -> Result<BTreeMap<String, Zeroizing<String>>> {
        let mut out = BTreeMap::new();
        for name in Envelope::list(conn)?
            .into_iter()
            .filter(|n| n.starts_with("keychain:"))
        {
            if self.0.is_none() {
                self.0 = Some(Envelope::os()?);
            }
            let envelope = self.0.as_mut().ok_or_else(|| {
                Error::Io("kith-core: the secrets envelope is unavailable".into())
            })?;
            let Some(bytes) = envelope.get(conn, &name)? else {
                continue;
            };
            // Checked in place: the one copy made is the Zeroizing one, and the error path holds none (`bytes` zeroes itself).
            let text = std::str::from_utf8(&bytes).map_err(|_| {
                Error::Refused(format!(
                    "authority: the secret {name} is not text; it cannot go back to the Keychain. Nothing was written back"
                ))
            })?;
            out.insert(name, Zeroizing::new(text.to_owned()));
        }
        Ok(out)
    }
}

/// The macOS Keychain as a [`OsStore`]: an item is named like the envelope's record, `keychain:SERVICE#ACCOUNT`.
struct KeychainStore;

fn split_item(name: &str) -> Result<(&str, &str)> {
    name.strip_prefix("keychain:")
        .and_then(|rest| rest.split_once('#'))
        .ok_or_else(|| {
            Error::Refused(format!(
                "authority: {name:?} is not a keychain:SERVICE#ACCOUNT item"
            ))
        })
}

impl OsStore for KeychainStore {
    fn get(&self, name: &str) -> Result<Option<Zeroizing<String>>> {
        let (service, account) = split_item(name)?;
        match Keychain.read(service, account) {
            Item::Found(v) => Ok(Some(v)),
            Item::Missing => Ok(None),
            Item::Unavailable(why) => Err(Error::Io(format!("authority: {name}: {why}"))),
        }
    }

    fn put(&mut self, name: &str, value: &str) -> Result<()> {
        if !cfg!(target_os = "macos") {
            return Err(Error::Io(
                "authority: no Keychain on this operating system to write back to".into(),
            ));
        }
        let (service, account) = split_item(name)?;
        keystore::put_generic_password(service, account, value)?;
        // `security -i` says nothing about a failure: the item has to read back as what was written.
        if self.get(name)?.is_some_and(|v| v.as_str() == value) {
            Ok(())
        } else {
            Err(Error::Io(format!(
                "authority: the Keychain did not keep {name} (locked, or an access prompt was refused)"
            )))
        }
    }
}

pub(crate) struct AuthorityModule {
    /// Where `import` reads the Keychain items the profiles name.
    source: Box<dyn ItemSource>,
    /// Where `import` seals them.
    sink: RefCell<Box<dyn SecretSink>>,
    /// Where `restore_secrets` reads them from.
    vault: RefCell<Box<dyn Vault>>,
    /// Where `restore_secrets` writes them to.
    os: RefCell<Box<dyn OsStore>>,
    /// The OS store's blob limit (the Windows Credential Manager's 2560 bytes); none on macOS and Linux.
    limit: Option<usize>,
}

impl AuthorityModule {
    pub(crate) fn real() -> Self {
        Self::with(
            Box::new(Keychain),
            Box::new(OsEnvelope::default()),
            Box::new(EnvelopeVault::default()),
            Box::new(KeychainStore),
            None,
        )
    }

    pub(crate) fn with(
        source: Box<dyn ItemSource>,
        sink: Box<dyn SecretSink>,
        vault: Box<dyn Vault>,
        os: Box<dyn OsStore>,
        limit: Option<usize>,
    ) -> Self {
        Self {
            source,
            sink: RefCell::new(sink),
            vault: RefCell::new(vault),
            os: RefCell::new(os),
            limit,
        }
    }
}

fn ctx(env: &Env) -> Result<&Ctx> {
    env.ctx.as_ref().ok_or_else(|| {
        Error::BadParams(
            "authority: this caller gave no folders (home, kernel), so auth.json cannot be found"
                .into(),
        )
    })
}

fn stat_line(p: &Path) -> String {
    match std::fs::metadata(p) {
        Ok(md) => {
            let mtime = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_nanos());
            format!("{}:{}:{mtime};", p.display(), md.len())
        }
        Err(_) => format!("{}:none;", p.display()),
    }
}

/// `serve.db` of the caller's serve folder (the host's store, where the vault's old rows live).
fn serve_db(ctx: &Ctx) -> PathBuf {
    match &ctx.serve {
        kith_kernel::secrets::ServeDir::Fixed(d) => d.join("serve.db"),
        // The endpoint would run a program; the default place is enough for a fingerprint.
        kith_kernel::secrets::ServeDir::Endpoint { home, .. } => home.join(".kith/serve/serve.db"),
    }
}

fn hex16(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .take(8)
        .fold(String::new(), |mut s, b| {
            use std::fmt::Write as _;
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// The newest `<auth.json>.pre-cutover-<time>` beside the file, parsed. `Ok(None)`: there is none.
fn pre_cutover(auth: &Path) -> Option<(Value, PathBuf)> {
    let name = auth.file_name()?.to_str()?;
    let prefix = format!("{name}.pre-cutover-");
    let mut found: Vec<(i64, PathBuf)> = std::fs::read_dir(auth.parent()?)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().into_string().ok()?;
            let t = n.strip_prefix(&prefix)?.parse::<i64>().ok()?;
            Some((t, e.path()))
        })
        .collect();
    found.sort();
    found.into_iter().rev().find_map(|(_, p)| {
        let v: Value = serde_json::from_slice(&std::fs::read(&p).ok()?).ok()?;
        v.is_object().then_some((v, p))
    })
}

/// Replaces the keys of `row` that core.db holds (`keys`) with the ones `from` has, and leaves the others as they were.
fn overlay(base: Option<&Value>, from: &Value, keys: &[&str]) -> Value {
    let mut row: Map<String, Value> = base.and_then(Value::as_object).cloned().unwrap_or_default();
    for k in keys {
        row.shift_remove(*k);
    }
    for (k, v) in from.as_object().into_iter().flatten() {
        row.insert(k.clone(), v.clone());
    }
    Value::Object(row)
}

/// The file `kith-auth` reads, from core.db's tables (`snap`, as `authority::snapshot` reads them) laid over `base`, the
/// last file that held the other keys. Every person, row, revoke and invite in core.db is in the result; nothing the core
/// no longer has is.
pub(crate) fn merge_back(base: Option<Value>, snap: &Value) -> Value {
    let base = base.filter(Value::is_object);
    let old = |t: &str| base.as_ref().and_then(|b| b.get(t));
    let mut out: Map<String, Value> = base
        .as_ref()
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for t in EMPTY_TABLES {
        out.entry(*t).or_insert_with(|| Value::Object(Map::new()));
    }
    let mut people = Map::new();
    for (id, p) in snap
        .get("people")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        people.insert(
            id.clone(),
            overlay(old("people").and_then(|o| o.get(id)), p, PERSON_KEYS),
        );
    }
    out.insert("people".into(), Value::Object(people));
    let mut members = Map::new();
    for (agent, rows) in snap
        .get("members")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let mut kept = Map::new();
        for (person, r) in rows.as_object().into_iter().flatten() {
            let was = old("members")
                .and_then(|m| m.get(agent))
                .and_then(|a| a.get(person));
            kept.insert(person.clone(), overlay(was, r, MEMBER_KEYS));
        }
        members.insert(agent.clone(), Value::Object(kept));
    }
    out.insert("members".into(), Value::Object(members));
    let mut presets = Map::new();
    for (name, p) in snap
        .get("presets")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        presets.insert(
            name.clone(),
            overlay(old("presets").and_then(|o| o.get(name)), p, PRESET_KEYS),
        );
    }
    out.insert("presets".into(), Value::Object(presets));
    for t in ["invites", "retired"] {
        out.insert(
            t.into(),
            snap.get(t)
                .cloned()
                .unwrap_or_else(|| Value::Object(Map::new())),
        );
    }
    match snap.get("owner_bots_budget") {
        Some(v) => out.insert("owner_bots_budget".into(), v.clone()),
        None => out.shift_remove("owner_bots_budget"),
    };
    Value::Object(out)
}

impl Module for AuthorityModule {
    fn name(&self) -> &'static str {
        "authority"
    }

    fn tombstones(&self, env: &Env) -> Vec<PathBuf> {
        env.ctx.iter().map(|c| c.auth.clone()).collect()
    }

    fn import(&self, env: &Env, core: Option<&mut Core>, opts: Opts) -> Result<Vec<String>> {
        let ctx = ctx(env)?;
        if tombstone::is_tombstone(&ctx.auth) {
            return Err(Error::Refused(format!(
                "authority: {} is a tombstone, not an auth.json; there is nothing to import (kith rollback authority puts the file back)",
                ctx.auth.display()
            )));
        }
        let rep = crate::import::run_in(
            &env.layout,
            ctx,
            AuthorityOptions {
                dry_run: opts.dry_run,
                force: opts.force,
            },
            self.source.as_ref(),
            &mut **self.sink.borrow_mut(),
            core,
        )?;
        Ok(rep.lines)
    }

    fn same_answer(&self, env: &Env, db: &Connection) -> Result<Vec<String>> {
        let ctx = ctx(env)?;
        if let Some(other) = tombstone::module_of(&ctx.auth) {
            return Ok(vec![format!(
                "{} is a tombstone (module {other}), not an auth.json to compare with",
                ctx.auth.display()
            )]);
        }
        crate::import::same_answer(ctx, db)
    }

    fn export_back(&self, env: &Env, core: &mut Core) -> Result<Vec<String>> {
        let ctx = ctx(env)?;
        let snap = crate::authority::snapshot(core.conn())?;
        // A file that is already a real auth.json (a rollback run again after it wrote) keeps its other keys; else the
        // copy the tombstone kept.
        let current = std::fs::read(&ctx.auth)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .filter(|v| v.is_object() && v.get("kith_tombstone").is_none());
        let (base, from) = match current {
            Some(v) => (Some(v), None),
            None => match pre_cutover(&ctx.auth) {
                Some((v, p)) => (Some(v), Some(p)),
                None => (None, None),
            },
        };
        let mut lines = Vec::new();
        match &from {
            Some(p) => lines.push(format!(
                "auth.json: profiles, grants, env and each person's other keys come from {}",
                p.display()
            )),
            None if base.is_none() => lines.push(
                "auth.json: no earlier copy to take profiles, grants and env from; they are written empty (the Keychain items are written back separately)"
                    .into(),
            ),
            None => {}
        }
        let text = serde_json::to_string_pretty(&merge_back(base, &snap))
            .map_err(|e| Error::Io(format!("authority: auth.json: {e}")))?;
        let io = |e: std::io::Error| Error::Io(format!("authority: {}: {e}", ctx.auth.display()));
        let tmp = ctx.auth.with_extension("restore.tmp");
        // auth.json holds the owner's access rules: the temporary file is private from its first byte, and on disk before
        // the rename makes it the file.
        let _ = std::fs::remove_file(&tmp);
        let mut file = {
            let mut o = std::fs::OpenOptions::new();
            o.write(true).create_new(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut o, 0o600);
            o.open(&tmp).map_err(io)?
        };
        {
            use std::io::Write as _;
            file.write_all(format!("{text}\n").as_bytes()).map_err(io)?;
        }
        file.sync_all().map_err(io)?;
        drop(file);
        // (Windows has no mode at open: the check there is `make_private`'s.)
        crate::perms::make_private(&tmp)?;
        std::fs::rename(&tmp, &ctx.auth).map_err(io)?;
        let n = |t: &str| snap.get(t).and_then(Value::as_object).map_or(0, Map::len);
        lines.push(format!(
            "auth.json written again from core.db ({} people, {} agents with members, {} presets, {} invites, {} retired)",
            n("people"),
            n("members"),
            n("presets"),
            n("invites"),
            n("retired")
        ));
        Ok(lines)
    }

    /// Writes the sealed `keychain:` records back to the Keychain. An item that holds a different value than the sealed
    /// one is kept, because the Keychain may have moved on since the import (a key rotated after the step). That is
    /// right only while the import is the one writer of `keychain:` envelope records (checked: `import/items.rs`; nothing
    /// else in core/runtime, the host or kith-auth writes one). A core-side secret writer (the secrets lane) would break
    /// the rule: its newer envelope value would lose to an older Keychain one. Whoever adds one must change this step.
    /// An item that the Keychain no longer holds is written back, and named (as `kept` is): "read back none" cannot
    /// tell deleted after the step from never there.
    fn restore_secrets(&self, _env: &Env, core: &mut Core) -> Result<Vec<String>> {
        let records = self.vault.borrow_mut().keychain_records(core.conn())?;
        if records.is_empty() {
            return Ok(vec![
                "secrets: no Keychain item in the envelope to write back".into(),
            ]);
        }
        // `security -i` parses a line: a name it cannot take as one word is found before any item is written.
        for name in records.keys() {
            let (service, account) = split_item(name)?;
            if !keystore::is_one_word(service) || !keystore::is_one_word(account) {
                return Err(Error::Refused(format!(
                    "authority: {name} has a space, quote or control character in its name; it cannot be written back to the Keychain. Nothing was written back"
                )));
            }
        }
        let mut os = self.os.borrow_mut();
        // The Keychain may have moved on since the import sealed its copy (a key rotated after the step): an item that
        // holds a different value is kept as it is. Only its NAME is reported.
        let mut kept = Vec::new();
        let mut returned = Vec::new();
        let mut items: BTreeMap<String, String> = BTreeMap::new();
        for (name, value) in records {
            match secrets_back::read_back(&**os, &name)? {
                Some(current) if current.as_str() != value.as_str() => {
                    kept.push(format!(
                        "secret kept: {name} (the Keychain holds a different value than the sealed one; left as it is)"
                    ));
                }
                found => {
                    if found.is_none() {
                        returned.push(format!(
                            "secret restored: {name} (the Keychain no longer holds it; written back from the sealed copy)"
                        ));
                    }
                    items.insert(name, value.to_string());
                }
            }
        }
        let done = secrets_back::restore_all(&mut **os, &items, self.limit);
        items.values_mut().for_each(String::zeroize);
        let mut lines = done?;
        lines.extend(kept);
        lines.extend(returned);
        Ok(lines)
    }

    fn digest(&self, _env: &Env, db: &Connection) -> Result<String> {
        Ok(hex16(
            crate::authority::snapshot(db)?.to_string().as_bytes(),
        ))
    }

    /// The old stores' stat lines and the number of vault rows, plus a digest of core.db's own authority tables. The
    /// digest is what makes a core-only write (a revoke, add or remove done through the core) change the fingerprint, so
    /// a cached `ok` for the opt-in cannot outlive it. Chosen over `PRAGMA data_version` (which does not move for writes
    /// made on the asking connection itself, so it can miss the daemon's own writes) and over a counter (every writer of
    /// the tables would have to bump it in its transaction, and one that forgot would leave the hole open). The digest
    /// reads the same tables `same_answer` reads, so it moves exactly when the compare could answer differently; it costs
    /// one read of small tables per ask, which the 2 s poll can afford.
    fn fingerprint(&self, env: &Env, db: &Connection) -> Result<String> {
        let core = self.digest(env, db)?;
        let db = serve_db(ctx(env)?);
        let wal = PathBuf::from(format!("{}-wal", db.display()));
        let vault_rows =
            Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .and_then(|c| {
                    c.query_row(
                        "SELECT COUNT(*) FROM kv WHERE k LIKE 'vault:v1:%'",
                        [],
                        |r| r.get::<_, i64>(0),
                    )
                })
                .map_or_else(|_| "novault".to_owned(), |n| n.to_string());
        Ok(format!(
            "{}{}vault:{vault_rows};core:{core}",
            stat_line(&db),
            stat_line(&wal)
        ))
    }
}
