//! The `people.*` rows the core answers (M2-2: the owner's writes to authority; M2-3: the access requests). The daemon routes them here (they need
//! `&mut Core`: a write is a claim transaction, as the gate's are). The params were checked against the row already.
//!
//! Answers are `{"lines": [...]}`, the lines `kith-auth` prints, so a CLI that switches to this prints them as they were;
//! `people.list` and `people.invites` answer `{"rows": [...]}` instead, and `people.invite` also names the `code`.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use kith_kernel::secrets::{self, Ctx, Die, Rules, Sandbox, ServeDir};
use serde_json::{Value, json};

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
            | "people.may"
            | "people.resolve"
            | "people.bridgeHeld"
            | "people.addBot"
            | "people.removeBot"
            | "people.botsCap"
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

pub(crate) fn ctx_of(core: &Core, params: &Value) -> Result<Ctx> {
    let (folders, kernel) = core.folders_for(params)?;
    let home = params
        .get("home")
        .and_then(Value::as_str)
        .map_or_else(kith_kernel::paths::process_home, PathBuf::from);
    Ok(Ctx {
        auth: folders.secrets.join("auth.json"),
        agents: folders.agents.clone(),
        kernel,
        person: None,
        now_ms: secrets::now_ms(),
        sandbox: Sandbox::Fixed(false),
        serve: ServeDir::Fixed(serve_dir_of(params, &home)),
    })
}

fn life_of(core: &Core, params: &Value) -> Result<Life> {
    let (folders, _) = core.folders_for(params)?;
    Ok(Life {
        agents: folders.agents.clone(),
        state: folders.state.clone(),
        machine: folders.machine.clone(),
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
    let (res, held, wants) = store::read(core, |a| asked(a))?.map_err(|d| store::refusal(&d))?;
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
    match method {
        "people.list" => store::read(
            core,
            |a| json!({ "rows": write::list(a, c, word(params, "agent"), word(params, "person")) }),
        ),
        // The answer of `may` itself (M2-15): the host's event filter asks it per person and agent. A refusal of the
        // rules is a "no" with its words, as for `kith-auth may`.
        "people.may" => {
            let (person, agent) = (
                need(params, "person").map_err(|d| store::refusal(&d))?,
                need(params, "agent").map_err(|d| store::refusal(&d))?,
            );
            store::read(core, |a| {
                let rules = Rules::new(c, a);
                match rules.may(person, agent, None, false) {
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
                        json!({ "allowed": false, "why": d.msg, "owner": false, "preset": null })
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
        "people.invites" => store::read(core, |a| json!({ "rows": write::invites(a, c) })),
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
            let said = store::run_with(
                core,
                c,
                |a| write_in(m, params, c, life, a, &fresh),
                |tx, said| {
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
                    // An id an open removal names can not join again until its order has run.
                    if matches!(m, "people.addPerson" | "people.addBot")
                        && let Some(id) = word(params, "id")
                        && let Some(of) = crate::revoke_order::open_removal_naming(tx, id)?
                    {
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
            let removed = said
                .as_object_mut()
                .and_then(|o| o.remove("removedPerson"))
                .and_then(|v| v.as_str().map(str::to_owned));
            if let Some(id) = removed.as_deref() {
                #[allow(clippy::cast_possible_truncation)]
                match crate::revoke_order::revoke_everywhere(core, id, &[], c.now_ms as i64) {
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
            Ok(lines(write::bot_add(
                a,
                need(params, "id")?,
                word(params, "of"),
                &kv,
            )?))
        }
        "people.removeBot" => {
            // read before the row goes: a renamed bot's cards may still sit under its old ids
            let was = revoke::was_ids(a, need(params, "id")?);
            let mut out = lines(write::bot_remove(a, need(params, "id")?)?);
            out["was"] = json!(was);
            Ok(out)
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
            Ok(lines(revoke::person_set(
                a,
                life,
                need(params, "id")?,
                &kv,
                switch(params, "owner"),
            )?))
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
