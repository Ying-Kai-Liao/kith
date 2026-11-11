//! The `people.*` rows the core answers (M2-2: the owner's writes to authority; M2-3: the access requests). The daemon routes them here (they need
//! `&mut Core`: a write is a claim transaction, as the gate's are). The params were checked against the row already.
//!
//! Answers are `{"lines": [...]}`, the lines `kith-auth` prints, so a CLI that switches to this prints them as they were;
//! `people.list` and `people.invites` answer `{"rows": [...]}` instead, and `people.invite` also names the `code`.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use kith_kernel::secrets::{self, Ctx, Die, Rules, Sandbox, ServeDir};
use serde_json::{Value, json};

use super::people_json;
use super::requests;
use super::revoke::{self, Life};
use super::store;
use super::write::{self, Settings};
use crate::db::Core;
use crate::error::{Error, Result};

/// `people.*` methods this module answers.
pub(crate) fn serves(method: &str) -> bool {
    matches!(
        method,
        "people.grant"
            | "people.revoke"
            | "people.list"
            | "people.listPeople"
            | "people.may"
            | "people.resolve"
            | "people.bridgeHeld"
            | "people.who"
            | "people.granted"
            | "people.addBot"
            | "people.removeBot"
            | "people.botsCap"
            | "people.budgets"
            | "people.ledger"
            | "people.removeGrant"
            | "people.handle"
            | "people.invite"
            | "people.invites"
            | "people.removeInvite"
            | "people.preset"
            | "people.removePreset"
            | "people.addPerson"
            | "people.renamePerson"
            | "people.removePerson"
            | "people.suspend"
            | "people.undo"
            | "people.clearTemp"
            | "people.retired"
            | "people.agentAccess"
            | "people.requests"
            | "people.approve"
            | "people.deny"
            | "people.fileRequest"
    )
}

/// Where kith serve keeps `cloud-sync.json` for this caller: its `KITH_SERVE_DIR`, else `<its home>/.kith/serve`
/// (what `kernel/bin/kith-serve` does). Not `<machine>/serve`: the machine folder is not always `~/.kith`, and a wrong
/// guess reads "never linked", which stops an out-of-sync Mac holding guests. Worked out before the transaction, so no
/// `kith-serve` process runs under the write lock.
fn serve_dir_of(params: &Value, home: &Path) -> PathBuf {
    let caller_env = params.get("home").is_some() || params.get("env").is_some();
    let given = if caller_env {
        params
            .get("env")
            .and_then(Value::as_object)
            .and_then(|e| e.get("KITH_SERVE_DIR"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    } else {
        std::env::var("KITH_SERVE_DIR").ok()
    };
    given
        .filter(|d| !d.is_empty())
        .map_or_else(|| home.join(".kith/serve"), PathBuf::from)
}

/// The caller's home: the one it names, else `process` (the process's own, when it can be known). Never a relative
/// path: an empty `$HOME` would make the serve folder `.kith/serve` under the working directory, which holds no
/// `cloud-sync.json` and reads "never linked", which fails open. Refused instead.
pub(super) fn home_of(params: &Value, process: Option<PathBuf>) -> Result<PathBuf> {
    let home = match params.get("home").and_then(Value::as_str) {
        Some(h) => PathBuf::from(h),
        None => process.unwrap_or_default(),
    };
    if home.is_absolute() {
        Ok(home)
    } else {
        Err(Error::Refused(
            "authority: no home folder to find the serve folder in (HOME is unset and the account has none)".into(),
        ))
    }
}

pub(crate) fn ctx_of(core: &Core, params: &Value) -> Result<Ctx> {
    let (folders, kernel) = core.folders_for(params)?;
    let home = home_of(params, kith_kernel::paths::known_home())?;
    let mut ctx = ctx_from(&folders, kernel);
    ctx.serve = ServeDir::Fixed(serve_dir_of(params, &home));
    Ok(ctx)
}

/// The rules' context over a caller's folders (also the cutover module's, which has no params to read them from; its
/// serve folder is the machine's, `ctx_of` sets the caller's).
pub(crate) fn ctx_from(folders: &kith_kernel::paths::Paths, kernel: PathBuf) -> Ctx {
    Ctx {
        auth: folders.secrets.join("auth.json"),
        agents: folders.agents.clone(),
        kernel,
        person: None,
        now_ms: secrets::now_ms(),
        sandbox: Sandbox::Fixed(false),
        serve: ServeDir::Fixed(folders.machine.join("serve")),
    }
}

fn life_of(core: &Core, params: &Value) -> Result<Life> {
    let (folders, kernel) = core.folders_for(params)?;
    Ok(Life {
        agents: folders.agents.clone(),
        state: folders.state.clone(),
        machine: folders.machine.clone(),
        kernel,
    })
}

fn word<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(Value::as_str)
}

/// A required param: its absence is the request's fault, like a usage error of `kith-auth`.
fn need<'a>(params: &'a Value, key: &str) -> std::result::Result<&'a str, Die> {
    word(params, key).ok_or_else(|| Die {
        msg: format!("{key} is required"),
        code: 2,
    })
}

/// What a `viaSwitch` write is refused with while authority is not switched; the host reads it to fall back to kith-auth.
pub(crate) const NOT_SWITCHED: &str = "not switched: authority is back on auth.json";

/// How the authority module's cutover record reads for a marked write.
#[derive(PartialEq)]
enum Record {
    /// Phase `switched` or `rollingBack`. A rollback that failed or is stuck leaves the tombstone in auth.json and the
    /// phase at `RollingBack` (a running one can not coexist with the daemon: the lifetime lock excludes it, so
    /// `RollingBack` seen here is always a stuck one), and the core is still the only store there.
    Switched,
    /// The record exists but can not be read. A tombstone with a record like that means the record was damaged.
    Damaged,
    /// No record, or another phase (stepping, rolled back): the file is the store.
    Other,
}

fn authority_record(machine: &Path) -> Record {
    use crate::cutover::state::Phase;
    let layout = crate::layout::Layout::new(machine);
    match crate::cutover::state::read(&layout, "authority") {
        Ok(Some(s)) if matches!(s.phase, Phase::Switched | Phase::RollingBack) => Record::Switched,
        Ok(_) => Record::Other,
        Err(_) => Record::Damaged,
    }
}

/// A marked method that only takes access away. On a damaged record these still go through (the host sent them
/// because auth.json is a tombstone, and refusing one leaves access in place); a grant never does.
fn removal_only(method: &str) -> bool {
    matches!(
        method,
        "people.revoke" | "people.removeBot" | "people.removePerson" | "people.suspend"
    )
}

/// The methods that only look, which the host's guard marks with `viaSwitch` (`CoreMethod::marked`, `core_verdict`).
pub(crate) fn is_read(method: &str, params: &Value) -> bool {
    matches!(
        method,
        "people.list"
            | "people.listPeople"
            | "people.may"
            | "people.resolve"
            | "people.bridgeHeld"
            | "people.who"
            | "people.granted"
            | "people.invites"
            | "people.budgets"
            | "people.ledger"
            | "people.retired"
    ) || (method == "people.botsCap" && word(params, "amount").is_none())
}

fn switch(params: &Value, key: &str) -> bool {
    params.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// Whether an answer to an access request is limited to members' requests: unless the caller is the owner at the
/// computer (`local`), and always when it says `members`.
fn members_only(params: &Value) -> bool {
    switch(params, "members") || !switch(params, "local")
}

/// The `key=value` settings the params carry, a key being there when the caller gave it (an empty value clears it).
fn settings(params: &Value, keys: &[&str]) -> Settings {
    keys.iter()
        .filter_map(|k| word(params, k).map(|v| ((*k).to_owned(), v.to_owned())))
        .collect()
}

fn lines(said: Vec<String>) -> Value {
    let said = said.into_iter().map(Value::String).collect();
    json!({ "lines": Value::Array(said) })
}

/// A new `/link` code: six letters and digits with no 0/O or 1/I to misread, from the OS's randomness.
fn new_code() -> Result<String> {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut b = [0u8; 6];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .map_err(|e| Error::Io(format!("kith-core: no randomness for an invite code: {e}")))?;
    Ok(b.iter()
        .map(|x| char::from(ALPHABET[usize::from(*x) % ALPHABET.len()]))
        .collect())
}

/// `kith-auth resolve AGENT [--person P] [--via V] [--take-once]` (M2-1b): what a run gets, and `held`. With `takeOnce`
/// and `via`, a refused launch is asked again counting a "just this once" grant, and the grant is used up in the SAME
/// transaction that decided, so two launches cannot both ride one grant, and a removal between the card and the launch
/// still wins. Not while held: the caller asks again until in sync, and the grant goes to the launch that runs.
fn resolve(core: &mut Core, c: &Ctx, params: &Value) -> Result<Value> {
    let agent = need(params, "agent").map_err(|d| store::refusal(&d))?;
    let via = word(params, "via").filter(|v| !v.is_empty());
    let take = switch(params, "takeOnce") && via.is_some();
    let person_of = |rules: &Rules| {
        word(params, "person")
            .filter(|p| !p.is_empty())
            .map_or_else(|| rules.acting(), str::to_owned)
    };
    let asked = |a: &Value| -> std::result::Result<_, Die> {
        let rules = Rules::new(c, a);
        let person = person_of(&rules);
        let res = rules.resolve(agent, &person, via, false)?;
        let held = rules.bridge_held(&person);
        let wants = take && res.get("allowed") == Some(&Value::Bool(false)) && !held;
        Ok((res, held, wants))
    };
    let dies = switch(params, "dies");
    let (res, held, wants) = match store::read(core, |a| asked(a))? {
        Ok(asked) => asked,
        Err(d) if dies => return Ok(died(&d)),
        Err(d) => return Err(store::refusal(&d)),
    };
    if !wants {
        return Ok(answer(res, held));
    }
    let (res, held) = store::run(core, c, |a| {
        let (mut res, held, wants) = asked(a)?;
        if !wants {
            return Ok((res, held));
        }
        let person = person_of(&Rules::new(c, a));
        let via = via.unwrap_or_default();
        (res, _) = secrets::resolve_taking_once(c, a, agent, &person, via)?;
        Ok((res, held))
    })?;
    Ok(answer(res, held))
}

/// The rules have no answer (a member on a deleted preset): where `kith-auth` dies with this line and exit code, a caller that
/// asks with `dies` (its router, on the tombstone) is told them, so it can print the same and exit the same.
fn died(d: &Die) -> Value {
    json!({ "died": d.code, "why": d.msg })
}

fn answer(mut res: serde_json::Map<String, Value>, held: bool) -> Value {
    res.insert("held".to_owned(), held.into());
    Value::Object(res)
}

/// Answers a `people.*` method over this core, as the caller's view (`kernel`, `home`, `env` in the params) sees the
/// agent folders.
pub(crate) fn call(core: &mut Core, method: &str, params: &Value) -> Result<Value> {
    let c = ctx_of(core, params)?;
    let life = life_of(core, params)?;
    call_in(core, &c, &life, method, params)
}

/// [`call`] with the context given (a test fixes the clock).
#[cfg(test)]
pub(crate) fn call_with(core: &mut Core, c: &Ctx, method: &str, params: &Value) -> Result<Value> {
    call_in(core, c, &Life::beside(c), method, params)
}

/// [`call_with`] with the folders of a person's life given too.
pub(crate) fn call_in(
    core: &mut Core,
    c: &Ctx,
    life: &Life,
    method: &str,
    params: &Value,
) -> Result<Value> {
    // A host that read through the switch (its guard reads, `viaSwitch`) is refused unless authority is switched, like a
    // marked write below, so the host's cached "switched" is not part of the safety argument: right after a rollback
    // the file is the truth again, and this store may be missing a revoke that only reached the file. A damaged record
    // is refused too (a read answers from a store nobody can vouch for; the host then reads the file, which fails closed).
    if switch(params, "viaSwitch") && is_read(method, params) {
        let machine = core
            .folders()
            .map_or_else(|| life.machine.clone(), |f| f.machine.clone());
        if authority_record(&machine) != Record::Switched {
            return Err(Error::Refused(format!("authority: {NOT_SWITCHED}")));
        }
    }
    match method {
        "people.list" => store::read(
            core,
            |a| json!({ "rows": write::list(a, c, word(params, "agent"), word(params, "person")) }),
        ),
        // `kith-auth people --json`: every person, bots and revoked ones too, with their agent rows and handles.
        "people.listPeople" => {
            store::read(core, |a| json!({ "rows": people_json::people_json(a, c) }))
        }
        // The answer of `may` itself (M2-15): the host's event filter asks it per person and agent. A refusal of the
        // rules is a "no" with its words, as for `kith-auth may`.
        "people.may" => {
            let (person, agent) = (
                need(params, "person").map_err(|d| store::refusal(&d))?,
                need(params, "agent").map_err(|d| store::refusal(&d))?,
            );
            // `via` and `once` as `kith-auth may --via CALLER --once` takes them (kith-permit asks whether the caller may
            // call a helper for the person, a "just this once" counted); the answer is still a read.
            let via = word(params, "via").filter(|v| !v.is_empty());
            let once = switch(params, "once");
            let dies = switch(params, "dies");
            store::read(core, |a| {
                let rules = Rules::new(c, a);
                match rules.may(person, agent, via, once) {
                    Ok((allowed, why)) => {
                        // M2-16: the host's guard also needs to know the owner (never a guest) and the preset (a
                        // member answers their own cards), so `resolve`'s preset rides along. No preset when the
                        // rules have none to give; the caller then reads "not a member".
                        let owner = why == "owner";
                        let preset = (allowed && !owner)
                            .then(|| rules.resolve(agent, person, None, false).ok())
                            .flatten()
                            .and_then(|r| {
                                r.get("preset").and_then(Value::as_str).map(str::to_owned)
                            });
                        json!({ "allowed": allowed, "why": why, "owner": owner, "preset": preset })
                    }
                    Err(d) => {
                        if dies {
                            died(&d)
                        } else {
                            json!({ "allowed": false, "why": d.msg, "owner": false, "preset": null })
                        }
                    }
                }
            })
        }
        // The device-pairing access requests (M2-3): a table of their own, not `store::run`'s auth.json-shaped write.
        // Every call here is the owner's (one Unix user), so the core cannot tell the owner at the computer from the
        // host answering for a remote device. It defaults to the narrow answer: members' requests only, unless the
        // caller says `local` (M2-17).
        "people.requests" => requests::list(core, c, members_only(params)),
        "people.approve" => {
            let id = need(params, "id").map_err(|d| store::refusal(&d))?;
            requests::approve(core, c, id, members_only(params))
        }
        "people.deny" => {
            let id = need(params, "id").map_err(|d| store::refusal(&d))?;
            requests::deny(core, c, id, members_only(params))
        }
        "people.fileRequest" => {
            let id = need(params, "id").map_err(|d| store::refusal(&d))?;
            let device_key = need(params, "deviceKey").map_err(|d| store::refusal(&d))?;
            let int = |k: &str| params.get(k).and_then(Value::as_i64).unwrap_or_default();
            let new = requests::NewRequest {
                id,
                device_key,
                device_name: word(params, "deviceName").unwrap_or_default(),
                platform: word(params, "platform").unwrap_or_default(),
                email: word(params, "email").filter(|e| !e.is_empty()),
                person: word(params, "person").filter(|p| !p.is_empty()),
                created_at: int("createdAt"),
                expires_at: int("expiresAt"),
            };
            requests::file(core, c, &new, word(params, "code"))
        }
        "people.resolve" => resolve(core, c, params),
        // `kith-auth`'s `bridge_held`: the host's channels hold a person other than the owner while this Mac is out of
        // sync with the cloud. The answer is the cloud-sync file's, not a table of core.db.
        "people.bridgeHeld" => {
            let person = need(params, "person").map_err(|d| store::refusal(&d))?;
            store::read(
                core,
                |a| json!({ "held": Rules::new(c, a).bridge_held(person) }),
            )
        }
        // `kith-auth who <platform> <user-id>` and `granted` (M2-23): the reads kith-auth routes here on the tombstone.
        "people.who" => {
            let (platform, id) = (
                need(params, "platform").map_err(|d| store::refusal(&d))?,
                need(params, "id").map_err(|d| store::refusal(&d))?,
            );
            store::read(core, |a| Rules::new(c, a).who(platform, id))?
                .map_err(|d| store::refusal(&d))
        }
        "people.granted" => {
            let (agent, service) = (
                need(params, "agent").map_err(|d| store::refusal(&d))?,
                need(params, "service").map_err(|d| store::refusal(&d))?,
            );
            let grants = store::read(core, |a| Rules::new(c, a).granted(agent, service))?
                .map_err(|d| store::refusal(&d))?;
            Ok(json!({ "grants": grants }))
        }
        "people.invites" => store::read(core, |a| json!({ "rows": write::invites(a, c) })),
        // `kith-auth budgets AGENT --json`: the lines are that one JSON line, so a caller that reads the CLI's output reads
        // this one's. A budget that is no number is an error, never "no limit".
        "people.budgets" => {
            let agent = need(params, "agent").map_err(|d| store::refusal(&d))?;
            let said =
                store::read(core, |a| write::budgets(a, agent))?.map_err(|d| store::refusal(&d))?;
            Ok(json!({ "budgets": said, "lines": [said.to_string()] }))
        }
        // `kith-auth ledger [--person ID] [--agent AGENT] --json`: rows, and the same as the one JSON line.
        "people.ledger" => {
            let named = |k: &str| -> std::result::Result<Option<&str>, Die> {
                match word(params, k) {
                    Some(v) if v.is_empty() || v.starts_with('-') => Err(Die {
                        msg: "ledger [--person ID] [--agent AGENT] [--json] | ledger --rm <key>"
                            .into(),
                        code: 2,
                    }),
                    v => Ok(v),
                }
            };
            let (person, agent) = (
                named("person").map_err(|d| store::refusal(&d))?,
                named("agent").map_err(|d| store::refusal(&d))?,
            );
            let rows = store::read(core, |a| {
                let person = person.map(|p| secrets::canonical(a, p));
                write::ledger(a, c, person.as_deref(), agent)
            })?;
            Ok(json!({ "rows": rows, "lines": [Value::Array(rows.clone()).to_string()] }))
        }
        // `bots-cap --json` (kith-cost bots): the cap as written and every id the owner's bots spend under.
        "people.botsCap" if word(params, "amount").is_none() && switch(params, "json") => {
            store::read(core, write::bots_cap_json)?.map_err(|d| store::refusal(&d))
        }
        "people.botsCap" if word(params, "amount").is_none() => {
            store::read(core, |a| lines(vec![write::bots_cap_line(a)]))
        }
        "people.retired" => store::read(core, |a| {
            revoke::retired_line(a, life, word(params, "id").unwrap_or_default())
                .map(|said| json!({ "retired": !said.is_empty(), "lines": said }))
        })?
        .map_err(|d| store::refusal(&d)),
        "people.clearTemp" => {
            let pid = need(params, "id").map_err(|d| store::refusal(&d))?;
            let ids =
                store::read(core, |a| revoke::temp_ids(a, pid))?.map_err(|d| store::refusal(&d))?;
            // whoever asks has stopped their runs; what is left is a folder, no table
            for x in &ids {
                revoke::guest_tmp_remove(life, x);
            }
            Ok(json!({ "lines": [], "cleared": ids }))
        }
        m if serves(m) => {
            let fresh = if m == "people.invite" {
                new_code()?
            } else {
                String::new()
            };
            // A person cut off, removed or renamed takes their guest doors with it, in the same transaction (M2-14).
            // The core's own machine folder (where its cutover records are), not the caller's view of it.
            let machine = core
                .folders()
                .map_or_else(|| life.machine.clone(), |f| f.machine.clone());
            let said = store::run_with(
                core,
                c,
                |a| write_in(m, params, c, life, a, &fresh),
                |tx, said| {
                    // A host that routed this write here because auth.json held the tombstone says so (`viaSwitch`).
                    // Its look at the file and this write are two steps. The cutover lifetime lock is what
                    // excludes `kith rollback authority` while the daemon lives (the rollback takes it before it
                    // writes RollingBack), so this check is for a phase left behind (rolled back, never switched).
                    // RollingBack counts as switched: a stuck rollback keeps the tombstone, and removals must work.
                    // A tombstone with no Switched/RollingBack record, seen by a live daemon, means the record is
                    // damaged or removed. Damaged: a write that only removes access still goes through (refusing it
                    // would leave access in place), a grant is refused. Removed: everything is refused.
                    // The phase is read here, in the write's own transaction. A write without the marker
                    // (kith-auth's mirror, the terminal) keeps today's behaviour.
                    if switch(params, "viaSwitch") {
                        let ok = match authority_record(&machine) {
                            Record::Switched => true,
                            Record::Damaged => removal_only(m),
                            Record::Other => false,
                        };
                        if !ok {
                            return Err(Error::Refused(format!("authority: {NOT_SWITCHED}")));
                        }
                    }
                    // Read inside the transaction by `write_in` (not answers): whether an add names a new id, and the
                    // bot's line of owners.
                    let joining = take_flag(said, "joining");
                    let lineage = take_lineage(said);
                    revoke::follow_in_doors(tx, m, params, said, c.now_ms)?;
                    revoke::follow_in_cards(tx, m, params, said)?;
                    // A removal drops the bots' rows: record the person and the bot set (possibly empty) in this
                    // transaction, so a crash before the order has run loses nothing (it runs again at start).
                    let removed = said
                        .get("removedPerson")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    if let Some(id) = removed.as_deref() {
                        let bots: Vec<String> = said
                            .get("ids")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_str)
                            .filter(|x| *x != id)
                            .map(str::to_owned)
                            .collect();
                        crate::revoke_order::record_removal(tx, id, &bots, c.now_ms)?;
                    }
                    // A suspension keeps the rows, so its record names no bots (the table walk finds them); it
                    // means "suspended, order not finished" all the same.
                    if let Some(id) = said.get("suspendedPerson").and_then(Value::as_str) {
                        crate::revoke_order::record_suspend(tx, id, c.now_ms)?;
                    }
                    // An id an open removal names can not join again until its order has run (an edit of someone
                    // who is already in `people` is not joining). Nor can a rename or a bot removal move a name the
                    // order still has to find: the suspend row names no bots, the table walk finds them by `bot_of`.
                    let named = match m {
                        "people.addPerson" | "people.addBot" if joining => word(params, "id")
                            .map_or(Ok(None), |id| {
                                crate::revoke_order::open_removal_naming(tx, id)
                            })?,
                        "people.renamePerson" => word(params, "id").map_or(Ok(None), |id| {
                            crate::revoke_order::open_removal_naming(tx, id)
                        })?,
                        "people.removeBot" => {
                            let mut hit = None;
                            for x in &lineage {
                                hit = hit.or(crate::revoke_order::open_removal_naming(tx, x)?);
                            }
                            hit
                        }
                        _ => None,
                    };
                    if let Some(of) = named {
                        return Err(Error::Refused(format!(
                            "authority: finishing the revoke of {of} first, retry"
                        )));
                    }
                    Ok(())
                },
            )?;
            // The removal is committed: run the revoke order now. An error leaves the record, and the replay at start
            // and on the supervisor's pass runs it again, so the removal itself still stands.
            let mut said = said;
            let removed = said.as_object_mut().and_then(|o| {
                let gone = o.remove("removedPerson");
                let held = o.remove("suspendedPerson");
                gone.or(held).and_then(|v| v.as_str().map(str::to_owned))
            });
            if let Some(id) = removed.as_deref() {
                #[allow(clippy::cast_possible_truncation)]
                match crate::revoke_order::run_open(core, id, c.now_ms as i64) {
                    Ok(out) if !out.kill.is_empty() => {
                        said["kill"] = out
                            .kill
                            .iter()
                            .map(|k| json!({"claimId": k.claim_id, "pid": k.pid, "started": k.started}))
                            .collect();
                    }
                    Ok(_) => {}
                    Err(e) => said["revokePending"] = json!(e.to_string()),
                }
            }
            // The folders follow a rename once it is committed. `kith-auth --local` clears no temp folder: the
            // caller stops their runs first and then asks `people.clearTemp` (the `ids` of a suspend or a removal
            // say whose).
            if m == "people.renamePerson"
                && let (Some(old), Some(new)) = (word(params, "id"), word(params, "to"))
            {
                let (moved, notes) = revoke::move_life(life, old, new);
                return Ok(renamed(said, &moved, notes));
            }
            Ok(said)
        }
        _ => Err(Error::NotServed(format!(
            "kith-core: {method} is not answered in the core yet"
        ))),
    }
}

/// One write over the auth.json-shaped value: what the transaction runs, and what the tests run on a plain value to
/// hold core.db to. `code` is the new invite code, for `people.invite`.
#[cfg(test)]
pub(super) fn write_op(
    method: &str,
    params: &Value,
    c: &Ctx,
    a: &mut Value,
    code: &str,
) -> std::result::Result<Value, Die> {
    write_in(method, params, c, &Life::beside(c), a, code)
}

/// [`write_op`] with the folders of a person's life given (the rules of an id read them).
pub(super) fn write_in(
    method: &str,
    params: &Value,
    c: &Ctx,
    life: &Life,
    a: &mut Value,
    code: &str,
) -> std::result::Result<Value, Die> {
    match method {
        "people.grant" => {
            let kv = settings(
                params,
                &["preset", "mode", "budget", "deny", "model", "helpers"],
            );
            let said = write::member(
                a,
                c,
                need(params, "agent")?,
                need(params, "person")?,
                &kv,
                switch(params, "trusted"),
                false,
            )?;
            Ok(lines(said))
        }
        "people.revoke" => Ok(lines(write::member(
            a,
            c,
            need(params, "agent")?,
            need(params, "person")?,
            &Settings::new(),
            false,
            true,
        )?)),
        "people.addBot" => {
            let kv = settings(params, &["name", "provider"]);
            let joining = !write::people(a).contains_key(need(params, "id")?);
            let mut out = lines(write::bot_add(
                a,
                need(params, "id")?,
                word(params, "of"),
                &kv,
            )?);
            out["joining"] = json!(joining);
            Ok(out)
        }
        "people.removeBot" => {
            // read before the row goes: a renamed bot's cards may still sit under its old ids
            let was = revoke::was_ids(a, need(params, "id")?);
            let lineage = lineage_of(a, need(params, "id")?);
            let mut out = lines(write::bot_remove(a, need(params, "id")?)?);
            out["was"] = json!(was);
            out["lineage"] = json!(lineage);
            Ok(out)
        }
        "people.agentAccess" => Ok(lines(write::agent_access(
            a,
            c,
            need(params, "agent")?,
            need(params, "domain")?,
            switch(params, "remove"),
        )?)),
        "people.removeGrant" => Ok(lines(write::remove_grant(a, c, need(params, "key")?)?)),
        "people.handle" => {
            let uid = if switch(params, "remove") {
                None
            } else {
                Some(need(params, "uid")?)
            };
            Ok(lines(write::handle(
                a,
                need(params, "person")?,
                need(params, "platform")?,
                uid,
            )?))
        }
        "people.botsCap" => Ok(lines(write::bots_cap(a, need(params, "amount")?)?)),
        "people.invite" => {
            let said = write::invite_create(
                a,
                c,
                need(params, "person")?,
                params.get("minutes").and_then(Value::as_i64),
                code,
            )?;
            Ok(json!({ "code": code, "lines": said }))
        }
        "people.removeInvite" => Ok(lines(write::invite_remove(a, c, need(params, "code")?)?)),
        "people.preset" => {
            let kv = settings(params, &["mode", "deny"]);
            Ok(lines(write::preset_set(
                a,
                need(params, "name")?,
                &kv,
                switch(params, "trusted"),
                false,
            )?))
        }
        "people.removePreset" => Ok(lines(write::preset_set(
            a,
            need(params, "name")?,
            &Settings::new(),
            false,
            true,
        )?)),
        "people.addPerson" => {
            let kv = settings(params, &["name", "lang", "tz"]);
            let joining = !write::people(a).contains_key(need(params, "id")?);
            let mut out = lines(revoke::person_set(
                a,
                life,
                need(params, "id")?,
                &kv,
                switch(params, "owner"),
            )?);
            out["joining"] = json!(joining);
            Ok(out)
        }
        "people.renamePerson" => Ok(lines(revoke::person_rename(
            a,
            life,
            need(params, "id")?,
            need(params, "to")?,
        )?)),
        "people.removePerson" if switch(params, "unused") => Ok(lines(
            revoke::person_remove_unused(a, life, need(params, "id")?)?,
        )),
        "people.removePerson" => {
            let was = revoke::was_ids(a, need(params, "id")?);
            let (said, ids) = revoke::person_remove(a, life, c, need(params, "id")?)?;
            let mut out = lines(said);
            // What the write did, for the record and the order below: keyed on the removal, not on the method name.
            out["removedPerson"] = json!(need(params, "id")?);
            out["ids"] = json!(ids);
            out["was"] = json!(was);
            Ok(out)
        }
        "people.suspend" | "people.undo" => {
            let (said, ids) =
                revoke::person_suspend(a, c, need(params, "id")?, method == "people.suspend")?;
            let mut out = lines(said);
            if method == "people.suspend" {
                out["was"] = json!(revoke::was_ids(a, need(params, "id")?));
                // keyed on the suspension (the canonical id), for the record and the order after the commit
                out["suspendedPerson"] = json!(ids.first());
            }
            out["ids"] = json!(ids);
            Ok(out)
        }
        _ => Err(Die {
            msg: format!("{method} is not a write"),
            code: 2,
        }),
    }
}

/// `id` and the owners above it by `bot_of` (a loop stops at the repeat), read from the snapshot `auth`.
fn lineage_of(auth: &Value, id: &str) -> Vec<String> {
    let mut out = vec![id.to_owned()];
    while let Some(up) = secrets::parent_of(auth, out.last().map_or("", String::as_str)) {
        if out.contains(&up) {
            break;
        }
        out.push(up);
    }
    out
}

/// Takes the in-transaction `joining` flag out of a write's answer, so it never reaches the caller.
fn take_flag(said: &mut Value, key: &str) -> bool {
    said.as_object_mut()
        .and_then(|o| o.remove(key))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// Takes the in-transaction `lineage` out of a write's answer.
fn take_lineage(said: &mut Value) -> Vec<String> {
    said.as_object_mut()
        .and_then(|o| o.remove("lineage"))
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default()
}

/// The answer of a committed rename, with the folders that really moved and a note for each that did not.
fn renamed(mut said: Value, moved: &[String], notes: Vec<String>) -> Value {
    let mut out = Vec::new();
    for (i, l) in said["lines"].as_array().into_iter().flatten().enumerate() {
        let line = l.as_str().unwrap_or_default();
        match line.strip_suffix(revoke::RENAMED_TAIL) {
            Some(head) if i == 0 && !moved.is_empty() => out.push(format!(
                "{head}; moved their memory and files in {}{}",
                moved.join(", "),
                revoke::RENAMED_TAIL
            )),
            _ => out.push(line.to_owned()),
        }
    }
    out.extend(notes);
    said["lines"] = json!(out);
    said
}
