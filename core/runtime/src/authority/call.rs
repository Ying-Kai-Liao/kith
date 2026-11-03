//! The `people.*` rows the core answers (M2-2): the owner's writes to authority. The daemon routes them here (they need
//! `&mut Core`: a write is a claim transaction, as the gate's are). The params were checked against the row already.
//!
//! Answers are `{"lines": [...]}`, the lines `kith-auth` prints, so a CLI that switches to this prints them as they were;
//! `people.list` and `people.invites` answer `{"rows": [...]}` instead, and `people.invite` also names the `code`.

use std::io::Read as _;

use kith_kernel::secrets::{self, Ctx, Die, Sandbox, ServeDir};
use serde_json::{Value, json};

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
            | "people.addBot"
            | "people.removeBot"
            | "people.botsCap"
            | "people.invite"
            | "people.invites"
            | "people.removeInvite"
            | "people.preset"
            | "people.removePreset"
    )
}

fn ctx_of(core: &Core, params: &Value) -> Result<Ctx> {
    let (folders, kernel) = core.folders_for(params)?;
    Ok(Ctx {
        auth: folders.secrets.join("auth.json"),
        agents: folders.agents.clone(),
        kernel,
        person: None,
        now_ms: secrets::now_ms(),
        sandbox: Sandbox::Fixed(false),
        serve: ServeDir::Fixed(folders.machine.join("serve")),
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

/// Answers a `people.*` method over this core, as the caller's view (`kernel`, `home`, `env` in the params) sees the
/// agent folders.
pub(crate) fn call(core: &mut Core, method: &str, params: &Value) -> Result<Value> {
    let c = ctx_of(core, params)?;
    call_with(core, &c, method, params)
}

/// [`call`] with the context given (a test fixes the clock).
pub(crate) fn call_with(core: &mut Core, c: &Ctx, method: &str, params: &Value) -> Result<Value> {
    match method {
        "people.list" => store::read(
            core,
            |a| json!({ "rows": write::list(a, c, word(params, "agent"), word(params, "person")) }),
        ),
        "people.invites" => store::read(core, |a| json!({ "rows": write::invites(a, c) })),
        "people.botsCap" if word(params, "amount").is_none() => {
            store::read(core, |a| lines(vec![write::bots_cap_line(a)]))
        }
        m if serves(m) => {
            let fresh = if m == "people.invite" {
                new_code()?
            } else {
                String::new()
            };
            store::run(core, c, |a| write_op(m, params, c, a, &fresh))
        }
        _ => Err(Error::NotServed(format!(
            "kith-core: {method} is not answered in the core yet"
        ))),
    }
}

/// One write over the auth.json-shaped value: what the transaction runs, and what the tests run on a plain value to
/// hold core.db to. `code` is the new invite code, for `people.invite`.
pub(super) fn write_op(
    method: &str,
    params: &Value,
    c: &Ctx,
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
        "people.removeBot" => Ok(lines(write::bot_remove(a, need(params, "id")?)?)),
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
        _ => Err(Die {
            msg: format!("{method} is not a write"),
            code: 2,
        }),
    }
}
