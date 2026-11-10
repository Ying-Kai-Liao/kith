//! The `authority` module of the cutover framework (M2-18a): who may use what, moved from `auth.json` into core.db behind
//! the framework's `step` / `rollback` / `check` and the `cutover.mayServe` gate. The switch stays OFF: nothing here
//! flips by itself, and `KITH_CORE_AUTHORITY` (read once, in `mod.rs`) still has to be set AND pass the gate.
//!
//! | hook             | what it does here                                                                                    |
//! |------------------|------------------------------------------------------------------------------------------------------|
//! | `tombstones`     | the caller's `auth.json`. (The host vault's rows in serve.db are not tombstoned: the host moves them itself at its first unlock, M2-7.) |
//! | `import`         | `import::run_in` (M2-8, `kith kernel core-migrate --authority`): idempotent, a snapshot first, `--dry-run`, `--force`, the Keychain items sealed into the envelope |
//! | `same_answer`    | `import::same_answer`: per person owner, revoked (yes/no), `bot_of`, `was`; the `retired` ids; the order of people; then `check.rs`: `may`, `resolve`, `resolve_bot`, `cut_off` over every person x agent x via x once, at the caller's "now" (so a grant that has expired or not started yet counts as the answer it gives now). Also compared, by name: the profiles (person, lent to, browser, which variables and whether each source differs; never a value) and the grants (agent -> service -> profiles in order); the host guard does not read them, but `retired`, `person --rm --unused` and the rollback rest on them, and a core imported before they were kept (`secrets_stale`) is a difference. NOT compared: the contents of invites, presets, `owner_bots_budget`, a bot's caps, `env`, and when a revoke or a grant was written. None of them is read by the host guard today (`may`, `resolve`, `resolve_bot`, `cut_off`); the host reading one of them is the point at which it joins the compare |
//! | `fingerprint`    | serve.db and its WAL (size, mtime), the number of vault rows in it, and the digest of core.db's authority tables (so a write made only through the core changes it); the framework adds auth.json's inode, size and mtime |
//! | `digest`         | the first 16 hex of the SHA-256 of core.db's authority tables, as the marker stores it                |
//! | `export_back`    | core.db -> auth.json, the shape `kith-auth` reads: the tables from core.db (people, members, presets, invites, retired, `agent_access`, the budget, `profiles` and `grants`) over the pre-cutover copy's other keys (`env`, `once`, a person's `root` / `kith` / `agents` ...). The core wins for every key it holds: a profile or a grant deleted in the core does not come back from the copy. Only a core imported before lane B's tables were filled (`secrets_stale`) keeps the copy's `profiles` and `grants` |
//! | `restore_secrets`| the envelope's `keychain:` records -> the OS store, before `export_back`                              |
//!
//! # What the file cannot say once it is a tombstone
//! `export_back` needs the keys the core never held (`env`, `once`, a person's `root` / `kith` / `agents`; and `profiles` /
//! `grants` for a core imported before they were kept). They come from the copy `tombstone::ensure` kept beside the file
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
//! - `people.removePerson --unused` and the `retired` check: `handles` (m0015), `agent@<id>` grants and owned or lent
//!   profiles (`secret_grants` / `secret_profiles`, filled by the import now) are checked. A core imported before the
//!   import kept them (`secrets_stale`, m0022) refuses both until it is imported again. The writers still have to keep
//!   the tables current (`forget`, `bot_add` / `bot_remove`: another package) before these verbs are switched.
//! - `kith-auth agent-access ... [--rm]`: `people.agentAccess` writes the `agent_access` table (and `merge_back` takes it
//!   from the core), but only the write half is ported: the listing (`agent-access <agent> [--json]`, with the
//!   package-declared hosts) has no core row, and the CLI words differ (`domain <host>` there, positional here), so the
//!   verb stays on Python until the listing and the switch.
//!
//! These do not change what the gate (`may_serve`) serves, which is the READ side (`may`, `resolve`, the request guard);
//! they are why `step` stays refused. (Since the import fills `profiles` and `grants`, `export_back` takes them from the core.)

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use kith_kernel::secrets::Ctx;
use rusqlite::Connection;
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};
use zeroize::{Zeroize as _, Zeroizing};

use super::authority_preflight::{Preflight, run as run_preflight};
use super::secrets_back::{self, OsStore};
use super::{Env, Module, Opts, SourceLock, SwitchedFile, marker, tombstone};
use crate::AuthorityOptions;
use crate::db::Core;
use crate::error::{Error, Result};
use crate::import::items::{Item, ItemSource, Keychain, OsEnvelope, SecretSink};
use crate::secrets::{Envelope, keystore};

/// The person keys the core holds (the import's list): `export_back` takes them from core.db and everything else a person
/// has from the old copy. `handles` is one: if the core's row has none, the old copy's must not come back, or an id that
/// was freed and re-added would give the old person's telegram/slack/kith handles to the new one.
const PERSON_KEYS: &[&str] = &[
    "name", "lang", "tz", "owner", "bot_of", "revoked", "was", "provider", "handles",
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

/// How long a step or a rollback waits for `auth.json.lock` (a `kith-auth` command holds it for milliseconds).
const LOCK_WAIT: Duration = Duration::from_secs(30);

/// The old vault's records in serve.db's `kv` table: the value starts with this (the key is the slot's name).
const OLD_VAULT_PREFIX: &str = "vault:v1:";

/// Why a step is refused until M2-18 lifts it (the architect's gate list for #793: 1, 2, 4-8).
const STEP_REFUSAL: &str = "not stepped: the M2-18 gates are open: kith-auth does not read the tombstone or answer from the core, and the host does not decide from the gate once switched (1, 2); addBot/removeBot and removePerson --unused do not clear grants, profiles and handles in one transaction (4, 5); no core writer for agent_access, profiles, grants or env (6, 7); the revoke bridge (8); the 'already switched' branch re-tombstones a file a writer put back after the switch, with no sha check and no import (9); no core read verb with kith-auth `people --json`'s row shape (id, name, owner, bot_of, root, revoked/cut_off, revoked_at, agent rows, handles) for its readers: serve/host/src/remote/members.rs (revoke path), api/people.rs, kith_recall.py, kith-cost, kith people (10). `step authority --dry-run` and `check authority` work now";

/// `auth.json.lock`, held (flock, exclusive): the lock `kith-auth` and `kith_kernel::secrets` take for a read-modify-write.
/// Dropping it lets the lock go.
struct AuthLock(#[allow(dead_code, reason = "held for its flock")] std::fs::File);

fn hold_auth_lock(auth: &Path, wait: Duration) -> Result<AuthLock> {
    use fs2::FileExt as _;
    let io = |e: std::io::Error| Error::Io(format!("authority: {}: {e}", auth.display()));
    if let Some(parent) = auth.parent() {
        std::fs::create_dir_all(parent).map_err(io)?;
    }
    let mut name = auth.file_name().unwrap_or_default().to_os_string();
    name.push(".lock");
    let file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(auth.with_file_name(name))
        .map_err(io)?;
    let deadline = Instant::now() + wait;
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(AuthLock(file)),
            Err(e) if e.raw_os_error() == fs2::lock_contended_error().raw_os_error() => {
                if Instant::now() >= deadline {
                    return Err(Error::Refused(format!(
                        "authority: auth.json.lock is held by another writer (a kith-auth command?) for more than {} s; nothing was changed, run it again",
                        wait.as_secs().max(1)
                    )));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => return Err(io(e)),
        }
    }
}

/// Replaces the import's `core-migrate --force` advice when a crash repair meets rows that differ and no copy was kept:
/// the crash came before the tombstone's link, so the file is the newer side.
const REPAIR_FORCE: &str = "This is the repair of a switch that stopped before the tombstone: kith-auth kept writing auth.json after the \
     core took its rows, so the file is newer and wins. Run `kith migrate step authority --force` to replace the core's rows \
     with the file's (a backup of core.db is taken first).";

/// A crash repair that finds auth.json written since the step and the core moved as well: either side holds changes the
/// other lacks, so neither `--force` (file over core) nor a rollback (core lost) is safe to suggest.
const REPAIR_BOTH: &str = "both sides changed since the switch (auth.json was written by kith-auth, and the core took changes of its own); nothing was changed. Compare with `kith migrate check authority` and settle it by hand";

/// The same refusal for a marker with no `auth_sha`. No shipped Mac has such a marker (the step refusal was never lifted, so
/// none has stepped); it can only come from a development core.
const REPAIR_UNKNOWN: &str = "the marker has no record of the file the step imported and the core has changed since, so it can't be told which side is newer; nothing was changed. Can't tell: compare by hand with `kith migrate check authority`";

fn core_digest(db: &Connection) -> Result<String> {
    Ok(hex16(
        crate::authority::snapshot(db)?.to_string().as_bytes(),
    ))
}

/// What a real auth.json (sha `now`) means against the marker the switch left. The same sha as the seed's `auth_sha` is the
/// file the switch imported: the core is the newer side and only the tombstone is left. A changed file is `--force`'s to
/// import only while the core's digest is still the marker's; with both moved, neither side may win.
fn repair_verdict(db: &Connection, module: &str, now: &str) -> SwitchedFile {
    let sha = marker::seed(db, module)
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v["auth_sha"].as_str().map(str::to_owned));
    if sha.as_deref() == Some(now) {
        return SwitchedFile::Imported;
    }
    let Some((_, at_switch)) = marker::read(db, module) else {
        return SwitchedFile::Changed;
    };
    if core_digest(db).is_ok_and(|d| d == at_switch) {
        // An older marker with no sha lands here too: rows that differ from an unmoved core can only be the file's.
        return SwitchedFile::Changed;
    }
    SwitchedFile::Refuse(
        if sha.is_some() {
            REPAIR_BOTH
        } else {
            REPAIR_UNKNOWN
        }
        .to_owned(),
    )
}

/// The SHA-256 of the file's bytes; "missing" when it does not exist.
fn sha_of(path: &Path) -> Result<String> {
    use std::fmt::Write as _;
    match std::fs::read(path) {
        Ok(b) => Ok(Sha256::digest(&b).iter().fold(String::new(), |mut s, x| {
            let _ = write!(s, "{x:02x}");
            s
        })),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok("missing".to_owned()),
        Err(e) => Err(Error::Io(format!("authority: {}: {e}", path.display()))),
    }
}

const CHANGED: &str =
    "authority: auth.json changed during the step; nothing was tombstoned. Run it again";

/// The slots of serve.db's old vault records that the core's envelope does not hold yet. The host moves them at its first
/// unlock (M2-7) and leaves the old rows in place, so a row counts only while its slot is missing from the envelope; an
/// old-format record whose key cannot be read stays unmoved for good. `None`: there is no serve.db (or no `kv` table).
pub(super) fn unmoved_vault_rows(ctx: &Ctx, core: &Connection) -> Result<Option<Vec<String>>> {
    let db = serve_db(ctx);
    if !db.exists() {
        return Ok(None);
    }
    let io = |e: rusqlite::Error| {
        Error::Refused(format!(
            "authority: serve.db cannot be read to look for credential records that have not moved: {e}"
        ))
    };
    let serve =
        Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(io)?;
    serve.busy_timeout(Duration::from_secs(2)).map_err(io)?;
    let slots: Vec<String> =
        match serve.prepare("SELECT k FROM kv WHERE substr(v, 1, length(?1)) = ?1 ORDER BY k") {
            Ok(mut st) => st
                .query_map([OLD_VAULT_PREFIX], |r| r.get(0))
                .and_then(|rows| rows.collect::<rusqlite::Result<Vec<String>>>())
                .map_err(io)?,
            Err(rusqlite::Error::SqliteFailure(_, Some(m))) if m.contains("no such table") => {
                return Ok(None);
            }
            Err(e) => return Err(io(e)),
        };
    let sealed: BTreeSet<String> = Envelope::list(core)?.into_iter().collect();
    Ok(Some(
        slots.into_iter().filter(|s| !sealed.contains(s)).collect(),
    ))
}

/// Refuses while serve.db holds old vault records the envelope lacks: the import does not carry them, and a switch over
/// them would leave the host's credentials behind.
fn refuse_unmoved_vault(ctx: &Ctx, core: &Connection) -> Result<()> {
    match unmoved_vault_rows(ctx, core)? {
        Some(rows) if !rows.is_empty() => Err(Error::Refused(format!(
            "authority: serve.db still holds {} credential record(s) that have not moved into the core's vault ({}); unlock the vault in the app first so the host moves them. A record that is still here after that can never be moved (its legacy key is lost, or the row is corrupt): remove that credential in the app, then run this again",
            rows.len(),
            rows.join(", ")
        ))),
        _ => Ok(()),
    }
}

pub(crate) struct AuthorityModule {
    /// False in the real module: `step` refuses until M2-18 lifts it. The tests build a lifted one to run the inner step.
    lifted: bool,
    /// How long to wait for `auth.json.lock`.
    lock_wait: Duration,
    /// What the import read: auth.json's sha, checked again just before the tombstone and written into the seed beside the
    /// marker. (`None` outside a step.)
    imported: RefCell<Option<String>>,
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
        Self {
            lifted: false,
            ..Self::with(
                Box::new(Keychain),
                Box::new(OsEnvelope::default()),
                Box::new(EnvelopeVault::default()),
                Box::new(KeychainStore),
                None,
            )
        }
    }

    /// The same module with the step refusal back (tests of the refusal).
    #[cfg(test)]
    pub(crate) fn refusing(mut self) -> Self {
        self.lifted = false;
        self
    }

    #[cfg(test)]
    pub(crate) fn waiting(mut self, wait: Duration) -> Self {
        self.lock_wait = wait;
        self
    }

    pub(crate) fn with(
        source: Box<dyn ItemSource>,
        sink: Box<dyn SecretSink>,
        vault: Box<dyn Vault>,
        os: Box<dyn OsStore>,
        limit: Option<usize>,
    ) -> Self {
        Self {
            lifted: true,
            lock_wait: LOCK_WAIT,
            imported: RefCell::new(None),
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

/// The rows whose old-copy keys a rollback may bring back: those that existed when the switch was made and are the same
/// incarnation now. A person removed and added again while switched has a new `seq`, so a reused id (a bot, an `--unused`
/// id) gets none of the old row's keys, such as the handles of an ended member.
#[derive(Default)]
pub(crate) struct Keep {
    people: BTreeSet<String>,
    /// (agent, person): a membership is kept only with its person.
    members: BTreeSet<(String, String)>,
    presets: BTreeSet<String>,
}

/// The seed `step` leaves beside the marker: each person's `seq`, the memberships and the presets at the switch.
/// `auth_sha` is the sha of the auth.json bytes the step imported, so a crash repair can tell a never-touched file from one
/// `kith-auth` wrote after.
fn seed_of(db: &Connection, auth_sha: Option<&str>) -> Result<String> {
    let mut people = Map::new();
    let mut q = db.prepare("SELECT id, seq FROM people")?;
    for r in q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
        let (id, seq) = r?;
        people.insert(id, seq.into());
    }
    let mut members: Map<String, Value> = Map::new();
    let mut q = db.prepare("SELECT agent, person FROM memberships")?;
    for r in q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))? {
        let (agent, person) = r?;
        if let Value::Array(a) = members
            .entry(agent)
            .or_insert_with(|| Value::Array(Vec::new()))
        {
            a.push(person.into());
        }
    }
    let mut presets = Vec::new();
    let mut q = db.prepare("SELECT name FROM presets")?;
    for r in q.query_map([], |r| r.get::<_, String>(0))? {
        presets.push(Value::String(r?));
    }
    Ok(
        json!({"people": people, "members": members, "presets": presets, "auth_sha": auth_sha})
            .to_string(),
    )
}

/// What the seed lets back now. No seed (a marker from before seeds) or one that does not parse: nothing comes back,
/// which loses the old copy's other keys for those rows and gives no row anyone else's.
fn keep_from(db: &Connection, seed: Option<&str>) -> Result<Keep> {
    let Some(v) = seed.and_then(|t| serde_json::from_str::<Value>(t).ok()) else {
        return Ok(Keep::default());
    };
    let mut keep = Keep::default();
    let mut q = db.prepare("SELECT id, seq FROM people")?;
    for r in q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
        let (id, seq) = r?;
        if v["people"].get(&id).and_then(Value::as_i64) == Some(seq) {
            keep.people.insert(id);
        }
    }
    for (agent, persons) in v["members"].as_object().into_iter().flatten() {
        for p in persons
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if keep.people.contains(p) {
                keep.members.insert((agent.clone(), p.to_owned()));
            }
        }
    }
    keep.presets = v["presets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| n.as_str().map(str::to_owned))
        .collect();
    Ok(keep)
}

/// [`merge_back_with`] bringing every row's old keys back (the pure merge the tests of the other tables use).
#[cfg(test)]
pub(crate) fn merge_back(base: Option<Value>, snap: &Value) -> Value {
    merge_back_with(base, snap, None)
}

/// The file `kith-auth` reads, from core.db's tables (`snap`, as `authority::snapshot` reads them) laid over `base`, the
/// last file that held the other keys. Every person, row, revoke and invite in core.db is in the result; nothing the core
/// no longer has is.
/// `keep`: which rows may take the old copy's keys that core.db does not hold (`None`: all of them).
pub(crate) fn merge_back_with(base: Option<Value>, snap: &Value, keep: Option<&Keep>) -> Value {
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
            overlay(
                old("people")
                    .and_then(|o| o.get(id))
                    .filter(|_| keep.is_none_or(|k| k.people.contains(id))),
                p,
                PERSON_KEYS,
            ),
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
                .and_then(|a| a.get(person))
                .filter(|_| {
                    keep.is_none_or(|k| k.members.contains(&(agent.clone(), person.clone())))
                });
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
            overlay(
                old("presets")
                    .and_then(|o| o.get(name))
                    .filter(|_| keep.is_none_or(|k| k.presets.contains(name))),
                p,
                PRESET_KEYS,
            ),
        );
    }
    out.insert("presets".into(), Value::Object(presets));
    for t in ["invites", "retired", "agent_access"] {
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
    // Lane B (m0021): the core holds the profiles and grants, so what it no longer has does not come back from the old
    // copy. A database imported before the import filled them (`secrets_stale`, m0022) holds empty tables that say
    // nothing: it keeps the old copy's, as it did before.
    let owner = snap
        .get("people")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .find(|(_, p)| p.get("owner") == Some(&Value::Bool(true)))
        .map(|(id, _)| id.clone());
    if snap.get("secrets_stale").is_none() {
        out.insert(
            "profiles".into(),
            profiles_back(old("profiles"), snap.get("profiles"), owner.as_deref()),
        );
        out.insert(
            "grants".into(),
            grants_back(old("grants"), snap.get("grants")),
        );
    }
    Value::Object(out)
}

/// `profiles` of the file from the core's. A profile whose person the core holds as NULL is shared on purpose when the
/// old copy marked it `*` (the core keeps no mark, and for a login `kith-auth` would otherwise take it as the owner's);
/// the text `the owner` is the core's stand-in for an owner not yet named, which the file leaves unsaid.
/// `grants` of the file from the core's. The core keeps no mark for a list of one, which `kith-auth` treats differently
/// from the bare profile (`KITH_AUTH_PROFILE` picks from a list), so a list of exactly that profile in the old copy stays one.
fn grants_back(old: Option<&Value>, from: Option<&Value>) -> Value {
    let mut out = from.and_then(Value::as_object).cloned().unwrap_or_default();
    for (agent, by_service) in &mut out {
        for (service, g) in by_service.as_object_mut().into_iter().flatten() {
            let was = old
                .and_then(|o| o.get(agent))
                .and_then(|a| a.get(service))
                .and_then(Value::as_array);
            if let (Some(profile), Some([only])) = (g.as_str(), was.map(Vec::as_slice))
                && only.as_str() == Some(profile)
            {
                *g = Value::Array(vec![only.clone()]);
            }
        }
    }
    Value::Object(out)
}

fn profiles_back(old: Option<&Value>, from: Option<&Value>, owner: Option<&str>) -> Value {
    let mut out = Map::new();
    for (name, p) in from.and_then(Value::as_object).into_iter().flatten() {
        let mut p = p.clone();
        if let Some(row) = p.as_object_mut() {
            match row.get("person").and_then(Value::as_str) {
                Some("the owner") => {
                    row.shift_remove("person");
                }
                Some(named) => {
                    // the import worked this owner out; the old copy did not name one
                    let unnamed = old
                        .and_then(|o| o.get(name))
                        .is_some_and(|o| o.get("person").is_none());
                    if unnamed && owner == Some(named) {
                        row.shift_remove("person");
                    }
                }
                None => {
                    let shared = old
                        .and_then(|o| o.get(name))
                        .and_then(|o| o.get("person"))
                        .and_then(Value::as_str)
                        == Some("*");
                    if shared {
                        row.insert("person".into(), Value::String("*".into()));
                    }
                }
            }
        }
        out.insert(name.clone(), p);
    }
    Value::Object(out)
}

impl Module for AuthorityModule {
    fn name(&self) -> &'static str {
        "authority"
    }

    fn tombstones(&self, env: &Env) -> Vec<PathBuf> {
        env.ctx.iter().map(|c| c.auth.clone()).collect()
    }

    fn step_refusal(&self, _env: &Env) -> Option<String> {
        (!self.lifted).then(|| STEP_REFUSAL.to_owned())
    }

    fn dry_run_despite_refusal(&self) -> bool {
        true
    }

    /// `auth.json.lock` from before the import reads the file until the tombstone is in place. The vault rows are looked
    /// at under it, before the framework writes a snapshot or a record.
    fn source_lock(&self, env: &Env) -> Result<Option<SourceLock>> {
        let ctx = ctx(env)?;
        let held = hold_auth_lock(&ctx.auth, self.lock_wait)?;
        if !tombstone::is_tombstone(&ctx.auth) {
            let db = Connection::open_with_flags(
                &env.layout.db,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .map_err(|e| Error::Io(format!("authority: core.db cannot be read: {e}")))?;
            refuse_unmoved_vault(ctx, &db)?;
        }
        *self.imported.borrow_mut() = None;
        Ok(Some(Box::new(held)))
    }

    /// The writers' lock for a rollback: a `kith-auth` command that is mid-write finishes first. Taken before the
    /// rolling-back record and the Keychain writes, so a refusal at the lock writes nothing.
    fn rollback_lock(&self, env: &Env) -> Result<Option<SourceLock>> {
        let held = hold_auth_lock(&ctx(env)?.auth, self.lock_wait)?;
        Ok(Some(Box::new(held)))
    }

    fn before_tombstone(&self, env: &Env) -> Result<()> {
        let Some(want) = self.imported.borrow().clone() else {
            // A real step always recorded the sha at its import; none means the guard was skipped. Fail closed, so a
            // file that was never checked is never tombstoned. (The unlifted module never reaches a real step.)
            return if self.lifted {
                Err(Error::Refused(
                    "authority: the import did not record auth.json's sha, so the file cannot be checked before the tombstone; nothing was tombstoned, run it again".into(),
                ))
            } else {
                Ok(())
            };
        };
        if sha_of(&ctx(env)?.auth)? == want {
            Ok(())
        } else {
            Err(Error::Refused(CHANGED.into()))
        }
    }

    fn import(&self, env: &Env, core: Option<&mut Core>, opts: Opts) -> Result<Vec<String>> {
        let ctx = ctx(env)?;
        if tombstone::is_tombstone(&ctx.auth) {
            return Err(Error::Refused(format!(
                "authority: {} is a tombstone, not an auth.json; there is nothing to import (kith rollback authority puts the file back)",
                ctx.auth.display()
            )));
        }
        // Recorded only for a real step (under the source lock); a file that moves while it is read is not imported.
        let before = if core.is_some() {
            Some(sha_of(&ctx.auth)?)
        } else {
            None
        };
        // A step that finds its own marker is a crash repair (the switch got as far as the marker, then died before the
        // tombstone): the file is newer than the core there, which changes what `--force` means.
        let repair = core
            .as_deref()
            .is_some_and(|c| marker::read(c.conn(), self.name()).is_some());
        // The advice for rows that differ in a repair: `--force` only while the core has not moved since the switch.
        let advice = match (&before, core.as_deref()) {
            (Some(now), Some(c)) => match repair_verdict(c.conn(), self.name(), now) {
                SwitchedFile::Refuse(why) => format!("{why}."),
                _ => REPAIR_FORCE.to_owned(),
            },
            _ => REPAIR_FORCE.to_owned(),
        };
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
        )
        .map_err(|e| match e {
            Error::Refused(s) if repair && s.contains(crate::import::DIFFERS_HINT) => {
                Error::Refused(s.replace(crate::import::DIFFERS_HINT, &advice))
            }
            e => e,
        })?;
        if let Some(before) = before {
            if sha_of(&ctx.auth)? != before {
                return Err(Error::Refused(CHANGED.into()));
            }
            *self.imported.borrow_mut() = Some(before);
        }
        Ok(rep.lines)
    }

    fn preflight(
        &self,
        env: &Env,
        import: Option<&Result<Vec<String>>>,
    ) -> Result<Option<Preflight>> {
        let switched = super::state::read(&env.layout, "authority")?
            .is_some_and(|s| s.phase == super::state::Phase::Switched);
        if switched {
            return Ok(Some(Preflight {
                items: Vec::new(),
                notes: vec![
                    "preflight: authority is already switched; the core is the only store, nothing to count"
                        .to_owned(),
                ],
            }));
        }
        let db = env
            .layout
            .db
            .exists()
            .then(|| {
                Connection::open_with_flags(
                    &env.layout.db,
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
                )
                .map_err(|e| Error::Io(format!("authority: core.db cannot be read: {e}")))
            })
            .transpose()?;
        let refusal = import
            .and_then(|r| r.as_ref().err())
            .map(ToString::to_string);
        // A real step has no import to ask yet, and its own import refuses by itself.
        run_preflight(self, env, refusal, db.as_ref(), import.is_some()).map(Some)
    }

    fn same_answer(&self, env: &Env, db: &Connection) -> Result<Vec<String>> {
        let ctx = ctx(env)?;
        if let Some(other) = tombstone::module_of(&ctx.auth) {
            return Ok(vec![format!(
                "{} is a tombstone (module {other}), not an auth.json to compare with",
                ctx.auth.display()
            )]);
        }
        refuse_unmoved_vault(ctx, db)?;
        crate::import::same_answer(ctx, &env.layout, db)
    }

    fn export_back(&self, env: &Env, core: &mut Core) -> Result<Vec<String>> {
        let ctx = ctx(env)?;
        // Runs under `rollback_lock` (taken before the rolling-back record), so no writer starts a read-modify-write on
        // the file while it is replaced.
        let snap = crate::authority::snapshot(core.conn())?;
        // A real auth.json is never replaced: `rollback` does not call this over one, and a caller that does gets the file
        // back as it is (kith-auth may have written it since, and the core's rows are older). Only a tombstone, or no
        // file, is written over; the keys the core does not hold come from the copy the tombstone kept.
        if tombstone::is_real(&ctx.auth) {
            return Ok(vec![
                "auth.json is already a real file; left as it is".to_owned(),
            ]);
        }
        let keep = keep_from(
            core.conn(),
            marker::seed(core.conn(), self.name()).as_deref(),
        )?;
        let (base, from) = match pre_cutover(&ctx.auth) {
            Some((v, p)) => (Some(v), Some(p)),
            None => (None, None),
        };
        let mut lines = Vec::new();
        match &from {
            Some(p) => lines.push(format!(
                "auth.json: env and each person's other keys come from {}",
                p.display()
            )),
            None if base.is_none() => lines.push(
                "auth.json: no earlier copy to take env from (and profiles and grants, for a core imported before they were kept); they are written empty or as the core has them (the Keychain items are written back separately)"
                    .into(),
            ),
            None => {}
        }
        let text = serde_json::to_string_pretty(&merge_back_with(base, &snap, Some(&keep)))
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

    fn switched_file(&self, env: &Env, db: &Connection) -> Result<SwitchedFile> {
        Ok(repair_verdict(db, self.name(), &sha_of(&ctx(env)?.auth)?))
    }

    fn seed(&self, _env: &Env, db: &Connection) -> Result<Option<String>> {
        let sha = self.imported.borrow().clone();
        seed_of(db, sha.as_deref()).map(Some)
    }

    fn digest(&self, _env: &Env, db: &Connection) -> Result<String> {
        core_digest(db)
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
        let serve = serve_db(ctx(env)?);
        let wal = PathBuf::from(format!("{}-wal", serve.display()));
        // The rows the envelope lacks: a slot the host moves changes the number, so a cached `ok` cannot outlive it.
        let vault_rows = match unmoved_vault_rows(ctx(env)?, db) {
            Ok(Some(rows)) => rows.len().to_string(),
            Ok(None) => "novault".to_owned(),
            Err(_) => "unreadable".to_owned(),
        };
        Ok(format!(
            "{}{}vault:{vault_rows};core:{core}",
            stat_line(&serve),
            stat_line(&wal)
        ))
    }
}
