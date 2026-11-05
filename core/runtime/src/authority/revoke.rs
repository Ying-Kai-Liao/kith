//! The person-level writes (M2-4): adding, renaming and removing a person, revoking one (here called a suspension, to
//! keep it apart from `people.revoke`, which takes one agent away from one person), undoing it, and the ids a removal
//! retires. Each is a port of the part of `kernel/bin/kith-auth` it is named for (`cmd_person`, `rename`,
//! `remove_unused`, `revoke_person` and `retired`/`need_free`), refusals and words included. Python is the reference.
//!
//! A suspension is ONE fact on the person, `revoked` (and when: `revoked_at`), and nothing else. Their memberships,
//! `helper_once` entries, bots and invites are not touched, so there is nothing to put back: the readers (`may`,
//! `resolve`, `resolve_bot`, `cut_off`) refuse a revoked person, and a bot of theirs, before they look at any row, the
//! `*` row included; undo clears the fact and the same rows answer again. That is why undo restores exactly what they
//! had, and why an owner's change to the rows in between (taking an agent away, a preset deleted) is kept, never
//! undone by it: `kith-auth person X --unrevoke` is the same. The owner may also add rows while someone is
//! suspended (`member` writes the row and says it waits, as `kith-auth` does), and undo then gives those back too: it
//! restores what the rows say at that moment, not a copy taken at the revoke. The model is `verification/revoke/RevokeDoors.tla` ("rows
//! untouched": its `R4`, and `MayP`/`MayB`, which refuse before any row).
//!
//! Not here, with who owns it: what a revoke also ends on its own computer (the delegations and wakes they started,
//! `kith-ask --end` and `kith-sessions sweep`, and the runs those name: the run tokens are M2-13's, the doors M2-14's),
//! and the cloud's record of a revoke on the owner's other Macs (`revokePerson`, lane H: kith-auth's `--local` is
//! this module). The grants and profiles a person owns, which `forget`, a rename and `retired` also read in
//! `kith-auth`, are lane B's tables and are not in core.db.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::PathBuf;

use kith_kernel::secrets::{self, Ctx};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::write::{
    R, Settings, bots_of, die, entry, forget, is_bot, is_id, need_person, people, str_of, table,
    tables, truthy, usage, words,
};

/// Where kith keeps what is a person's outside the tables: agents' `memory/people/<id>` and `files/people/<id>`,
/// `<state>/people/<id>.md`, and the Claude Code temp folder of their runs under `<machine>/run/t`.
#[derive(Clone, Debug)]
pub(crate) struct Life {
    pub(crate) agents: PathBuf,
    pub(crate) state: PathBuf,
    pub(crate) machine: PathBuf,
}

impl Life {
    /// The usual layout, from the agents folder alone (for a caller that has no better view of the folders).
    #[cfg(test)]
    pub(crate) fn beside(c: &Ctx) -> Life {
        let home = c
            .agents
            .parent()
            .map_or_else(|| c.agents.clone(), std::path::Path::to_path_buf);
        Life {
            agents: c.agents.clone(),
            state: home.join("state"),
            machine: home.join("machine"),
        }
    }
}

fn check_id(id: &str) -> R<()> {
    if is_id(id) {
        Ok(())
    } else {
        usage("a person id is lowercase letters, digits and -, e.g. sam")
    }
}

/// `person_folders`: (label, path) of what kith keeps for a person outside the tables.
fn person_folders(life: &Life, pid: &str) -> Vec<(String, PathBuf)> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&life.agents)
        .map(|rd| rd.filter_map(Result::ok).map(|e| e.path()).collect())
        .unwrap_or_default();
    dirs.sort();
    let mut out = Vec::new();
    for d in dirs {
        let label = d
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        for sub in ["memory", "files"] {
            let p = d.join(sub).join("people").join(pid);
            if p.exists() {
                out.push((label.clone(), p));
            }
        }
    }
    let p = life.state.join("people").join(format!("{pid}.md"));
    if p.exists() {
        out.push(("people".to_owned(), p));
    }
    out
}

/// The labels of [`person_folders`], sorted and each once.
fn kept_labels(life: &Life, pid: &str) -> Vec<String> {
    person_folders(life, pid)
        .into_iter()
        .map(|(label, _)| label)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// `retired`: why a new person or bot can't take this id, or `None`. A removed person's id is retired; whoever took it
/// would read the old person's memory and files and use the logins they owned. `left` is false for a bot (its folders
/// are its own from an earlier join) and for the owner.
pub(super) fn retired(auth: &Value, life: &Life, pid: &str, left: bool) -> Option<String> {
    if auth.get("retired").and_then(|r| r.get(pid)).is_some() {
        return Some(format!("'{pid}' was a person who was removed"));
    }
    let canon = secrets::canonical(auth, pid);
    if canon != pid {
        return Some(format!("'{pid}' is an old id of {canon}"));
    }
    if !left {
        return None;
    }
    let kept = kept_labels(life, pid);
    if !kept.is_empty() {
        return Some(format!(
            "memory and files of a removed '{pid}' are still in {} (move them away to use the id)",
            kept.join(", ")
        ));
    }
    // TODO(lane B): `profiles` is not a core.db table, so this finds nothing yet; a reused id could inherit logins.
    let mut owned: Vec<&str> = auth
        .get("profiles")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter(|(_, prof)| str_of(prof, "person") == Some(pid))
        .map(|(n, _)| n.as_str())
        .collect();
    owned.sort_unstable();
    if !owned.is_empty() {
        return Some(format!("a removed '{pid}' still owns {}", owned.join(", ")));
    }
    None
}

/// `need_free`: for a new person or bot, refused when the id is retired.
fn need_free(auth: &Value, life: &Life, pid: &str, left: bool) -> R<()> {
    if let Some(why) = retired(auth, life, pid, left) {
        return die(format!(
            "{why}: give the new person another id (e.g. {pid}-2)"
        ));
    }
    Ok(())
}

/// `kith-auth retired <id>`: the reason, or nothing.
pub(crate) fn retired_line(auth: &Value, life: &Life, pid: &str) -> R<Vec<String>> {
    check_id(pid)?;
    Ok(retired(auth, life, pid, true).into_iter().collect())
}

/// A value as Python's `str()` shows it in an f-string.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => (if *b { "True" } else { "False" }).to_owned(),
        Value::Null => "None".to_owned(),
        Value::Number(n) => n.to_string(),
        other => py_repr(other),
    }
}

/// A value as Python's `repr()` shows it.
fn py_repr(v: &Value) -> String {
    match v {
        Value::String(s) => format!("'{s}'"),
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter()
                .map(|(k, x)| format!("'{k}': {}", py_repr(x)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => py_str(other),
    }
}

/// `kith-auth person <id> [name=.. lang=.. tz=..] [owner]`: add a person, or change one.
pub(crate) fn person_set(
    auth: &mut Value,
    life: &Life,
    pid: &str,
    kv: &Settings,
    owner: bool,
) -> R<Vec<String>> {
    check_id(pid)?;
    tables(auth);
    if owner && is_bot(auth, pid) {
        return die(format!("{pid} is a bot: it can not be the owner"));
    }
    if owner && truthy(people(auth).get(pid).and_then(|p| p.get("revoked"))) {
        // the owner is checked before a removal, so an owner whose bots kept a row would let them in while cut_off says no
        return die(format!(
            "{pid}'s access was removed: kith-auth person {pid} --unrevoke first"
        ));
    }
    let known = people(auth).contains_key(pid);
    let canon = secrets::canonical(auth, pid);
    if !known && canon != pid {
        return die(format!("'{pid}' was renamed to {canon}"));
    }
    if !known {
        need_free(auth, life, pid, !owner)?;
        // a new person starts with nothing, whatever an older removal left behind
        forget(auth, &[pid]);
    }
    {
        let p = entry(table(auth, "people"), pid);
        // in the order the line shows them, whatever order the caller named them in
        for k in ["name", "lang", "tz"] {
            match kv.get(k) {
                Some(v) if v.is_empty() => {
                    p.remove(k);
                }
                Some(v) => {
                    p.insert(k.to_owned(), v.clone().into());
                }
                None => {}
            }
        }
    }
    let mut out = Vec::new();
    if owner {
        let was = secrets::owner(auth);
        if let Some(was) = was.filter(|w| w != pid) {
            // TODO(lane B): kith-auth also pins the profiles that were the old owner's by default to them
            // (`kept <was>'s: ...`); profiles are not in core.db yet.
            out.push(format!(
                "note: anyone using this Mac account, and {was}'s KITH_DISCORD_OWNER_ID, still run as the owner"
            ));
        }
        for other in table(auth, "people").values_mut() {
            if let Some(o) = other.as_object_mut() {
                o.remove("owner");
            }
        }
        entry(table(auth, "people"), pid).insert("owner".to_owned(), true.into());
    }
    let shown: Vec<String> = people(auth)
        .get(pid)
        .and_then(Value::as_object)
        .map(|p| {
            p.iter()
                .filter(|(k, _)| k.as_str() != "handles" && k.as_str() != "was")
                .map(|(k, v)| format!("{k} {}", py_str(v)))
                .collect()
        })
        .unwrap_or_default();
    out.push(format!(
        "{pid}: {}",
        if shown.is_empty() {
            "added".to_owned()
        } else {
            shown.join(", ")
        }
    ));
    Ok(out)
}

/// `rename`: a person's new id, carried through every table that names them. The old id stays in `was`, so what
/// outside the tables still says old (a chat thread they started, the cost ledger) keeps meaning them. The folders
/// follow after the commit (`move_life`).
pub(crate) fn person_rename(auth: &mut Value, life: &Life, old: &str, new: &str) -> R<Vec<String>> {
    check_id(old)?;
    tables(auth);
    need_person(auth, old)?;
    check_id(new)?;
    let taken = people(auth).contains_key(new)
        || people(auth)
            .values()
            .any(|p| words(p.get("was")).iter().any(|w| w == new));
    if taken {
        return die(format!("'{new}' is already a person (or one's old id)"));
    }
    need_free(auth, life, new, true)?;
    forget(auth, &[new]);
    let mut p = table(auth, "people")
        .shift_remove(old)
        .unwrap_or(Value::Null);
    let mut was: Vec<Value> = Vec::new();
    for w in words(p.get("was")).into_iter().chain([old.to_owned()]) {
        let w = Value::String(w);
        if !was.contains(&w) {
            was.push(w);
        }
    }
    if let Some(o) = p.as_object_mut() {
        o.insert("was".to_owned(), Value::Array(was));
    }
    table(auth, "people").insert(new.to_owned(), p);
    for v in table(auth, "people").values_mut() {
        if str_of(v, "bot_of") == Some(old)
            && let Some(o) = v.as_object_mut()
        {
            o.insert("bot_of".to_owned(), new.into());
        }
    }
    for rows in table(auth, "members").values_mut() {
        let Some(rows) = rows.as_object_mut() else {
            continue;
        };
        if let Some(r) = rows.shift_remove(old) {
            rows.insert(new.to_owned(), r);
        }
        // who approved an always is them under the new id too (a demotion drops it)
        for row in rows.values_mut() {
            if let Some(by) = row.get_mut("access_by").and_then(Value::as_object_mut) {
                for v in by.values_mut() {
                    if v.as_str() == Some(old) {
                        *v = new.into();
                    }
                }
            }
        }
    }
    for inv in table(auth, "invites").values_mut() {
        if str_of(inv, "person") == Some(old)
            && let Some(o) = inv.as_object_mut()
        {
            o.insert("person".to_owned(), new.into());
        }
    }
    Ok(vec![format!("{old} is now {new}{RENAMED_TAIL}")])
}

/// What a rename's line ends with; the folders that really moved are said before it (`renamed` in `call.rs`).
pub(super) const RENAMED_TAIL: &str =
    " (profile names stay as they are: they name accounts, not people)";

/// After the commit of a rename: their folders take the new id. Returns the labels of what moved and a note for each
/// folder left as it was (the destination exists, or the move failed): the write happened, so none of it is an error.
pub(super) fn move_life(life: &Life, old: &str, new: &str) -> (Vec<String>, Vec<String>) {
    let (mut moved, mut notes) = (BTreeSet::new(), Vec::new());
    for (label, p) in person_folders(life, old) {
        let dest = if p.extension().is_some_and(|e| e == "md") {
            p.with_file_name(format!("{new}.md"))
        } else {
            p.with_file_name(new)
        };
        if dest.exists() {
            notes.push(format!(
                "note: {} is already there; {} left as it is",
                dest.display(),
                p.display()
            ));
            continue;
        }
        match std::fs::rename(&p, &dest) {
            Ok(()) => {
                moved.insert(label);
            }
            Err(e) => notes.push(format!(
                "note: {} did not move to {}: {e}",
                p.display(),
                dest.display()
            )),
        }
    }
    (moved.into_iter().collect(), notes)
}

/// `person <id> --rm`: they go, with their bots, memberships and open invites; the id is retired (and the ids they
/// had), so no new person takes it. Their memory and files stay. Returns the lines and every id that went.
pub(crate) fn person_remove(
    auth: &mut Value,
    life: &Life,
    c: &Ctx,
    pid: &str,
) -> R<(Vec<String>, Vec<String>)> {
    check_id(pid)?;
    tables(auth);
    // their bots go with them: a bot is never more than its owner's, so without one it has no access anyway
    let mut gone = vec![pid.to_owned()];
    let mut i = 0;
    while i < gone.len() {
        for b in bots_of(auth, &gone[i]) {
            if !gone.contains(&b) {
                gone.push(b);
            }
        }
        i += 1;
    }
    let person = people(auth).get(pid).cloned();
    if let Some(p) = person.filter(|p| !truthy(p.get("bot_of"))) {
        // bots are not retired: their ids are the cloud's, and the same bot may join again
        #[allow(clippy::cast_possible_truncation)]
        let at = (c.now_ms / 1000.0) as i64;
        for x in std::iter::once(pid.to_owned()).chain(words(p.get("was"))) {
            table(auth, "retired").insert(x, json!({ "at": at }));
        }
    }
    for x in &gone {
        // shift_remove: a person leaving must not move the others (the first match of `canonical` and `owner` wins)
        table(auth, "people").shift_remove(x);
    }
    let ids: Vec<&str> = gone.iter().map(String::as_str).collect();
    forget(auth, &ids);
    let kept = kept_labels(life, pid);
    let mut msg = format!("{pid} removed, with their memberships and grants");
    if gone.len() > 1 {
        msg = format!("{msg} and bots ({})", gone[1..].join(", "));
    }
    msg.push_str(" (profiles they own are kept); the id is retired, a new person gets another");
    if !kept.is_empty() {
        msg = format!(
            "{msg}; their memory and files in {} are kept (delete them by hand)",
            kept.join(", ")
        );
    }
    Ok((vec![msg], gone))
}

/// `person <id> --rm --unused`: only kith serve knows that no invite link went out, so it says so; this is the
/// backstop, under the same transaction as the removal. Anything the person has (or a bot, an old id, memory, a
/// `/link` code) refuses, and the id is not retired.
pub(crate) fn person_remove_unused(auth: &mut Value, life: &Life, pid: &str) -> R<Vec<String>> {
    check_id(pid)?;
    tables(auth);
    need_person(auth, pid)?;
    // TODO(lane B): kith-auth also refuses a person who has `handles` (core.db has no such column yet), `agent@<id>`
    // grants or owned/shared profiles; none of those tables are here. Until they are checked in this transaction,
    // `--rm --unused` must not be switched (see docs/plans/one-bot-model.md).
    let p = people(auth).get(pid).cloned().unwrap_or(Value::Null);
    let mut has: Vec<String> = ["owner", "bot_of", "handles", "was", "revoked"]
        .into_iter()
        .filter(|k| truthy(p.get(*k)))
        .map(str::to_owned)
        .collect();
    if !bots_of(auth, pid).is_empty() {
        has.push("bots".to_owned());
    }
    let member = auth
        .get("members")
        .and_then(Value::as_object)
        .is_some_and(|m| m.values().any(|rows| rows.get(pid).is_some()));
    if member {
        has.push("memberships".to_owned());
    }
    let invited = auth
        .get("invites")
        .and_then(Value::as_object)
        .is_some_and(|m| m.values().any(|i| str_of(i, "person") == Some(pid)));
    if invited {
        has.push("invites".to_owned());
    }
    has.extend(kept_labels(life, pid));
    if !has.is_empty() {
        return die(format!(
            "{pid} is not unused ({}): kith-auth person {pid} --rm removes them and retires the id",
            has.join(", ")
        ));
    }
    table(auth, "people").shift_remove(pid);
    Ok(vec![format!(
        "{pid} removed (unused: the id is free again)"
    )])
}

/// `root_of`: follow `bot_of` to the person at the top; a loop stops at the repeat.
fn root_of(auth: &Value, person: &str) -> String {
    let mut person = person.to_owned();
    let mut seen = vec![person.clone()];
    while let Some(parent) = secrets::parent_of(auth, &person) {
        if seen.contains(&parent) {
            break;
        }
        seen.push(parent.clone());
        person = parent;
    }
    person
}

/// `revoke_person`, the local write (`--local`): the person stays in kith (rows, handles, bots, memory) and reaches
/// nothing; undo clears the one fact. Revoking twice leaves the first time; undoing what was never revoked changes
/// nothing. Returns the lines, and the ids whose runs end with it (the person and their bots).
pub(crate) fn person_suspend(
    auth: &mut Value,
    c: &Ctx,
    pid: &str,
    revoke: bool,
) -> R<(Vec<String>, Vec<String>)> {
    check_id(pid)?;
    tables(auth);
    let pid = secrets::canonical(auth, pid);
    need_person(auth, &pid)?;
    // Only the revoke is refused: undo widens nothing (the owner has everything) and clears an old owner-and-revoked row.
    if revoke && secrets::is_owner(auth, &pid) {
        return die(format!("{pid} is the owner: the owner can not be revoked"));
    }
    if is_bot(auth, &pid) {
        return die(format!(
            "{pid} is a bot: revoke its owner {} (its bots go dormant), or remove it (kith-auth bot {pid} --rm)",
            root_of(auth, &pid)
        ));
    }
    {
        let p = entry(table(auth, "people"), &pid);
        if revoke {
            if !truthy(p.get("revoked")) {
                // An open /link code stays: link refuses a revoked person's code (cut_off), and undo gives it back
                // while it lasts.
                #[allow(clippy::cast_possible_truncation)]
                let at = (c.now_ms / 1000.0) as i64;
                p.insert("revoked".to_owned(), json!({ "at": at }));
            }
        } else {
            p.remove("revoked");
        }
    }
    let bots = bots_of(auth, &pid);
    let line = if revoke {
        format!(
            "{pid}: access removed; their rows, accounts{} are kept (undo: kith-auth person {pid} --unrevoke)",
            if bots.is_empty() {
                String::new()
            } else {
                format!(" and bots ({}, dormant)", bots.join(", "))
            }
        )
    } else {
        format!(
            "{pid}: access back as it was{}",
            if bots.is_empty() {
                String::new()
            } else {
                format!(", bots too ({})", bots.join(", "))
            }
        )
    };
    let mut ids = vec![pid];
    ids.extend(bots);
    Ok((vec![line], ids))
}

/// What a person-level write takes with it in the guest doors' table (M2-14), in the write's own transaction: a
/// suspension, a removal and a bot's removal sweep the open doors of the ids they name (the person and their bots; never
/// the owner, whom none of them can name), and a rename carries the doors to the new id. Undo does nothing: a swept
/// door stays swept. The swept doors go into the answer under `doors`, for the caller to stop their boxes.
pub(super) fn follow_in_doors(
    conn: &rusqlite::Connection,
    method: &str,
    params: &Value,
    said: &mut Value,
    now_ms: f64,
) -> crate::error::Result<()> {
    #[allow(clippy::cast_possible_truncation)]
    let now = now_ms as i64;
    let ids: Vec<String> = match method {
        "people.suspend" | "people.removePerson" if said.get("ids").is_some() => said["ids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|x| x.as_str().map(str::to_owned))
            .collect(),
        "people.removeBot" => params
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .into_iter()
            .collect(),
        "people.renamePerson" => {
            if let (Some(old), Some(new)) = (
                params.get("id").and_then(Value::as_str),
                params.get("to").and_then(Value::as_str),
            ) {
                crate::doors::renamed(conn, old, new)?;
            }
            return Ok(());
        }
        _ => return Ok(()),
    };
    let why = if method == "people.removeBot" {
        "its bot was removed"
    } else if method == "people.removePerson" {
        "its person was removed"
    } else {
        "its person was cut off"
    };
    let doors = crate::doors::sweep_ids(conn, &ids, why, now)?;
    said["doors"] = Value::Array(doors.iter().map(crate::doors::Door::json).collect());
    Ok(())
}

/// What a suspension, a removal and a bot's removal take with them in the card store (M2-11b, A7): the open cards and
/// the unused once-grants (`approved`) of the ids they name, the person and their bots, in the write's own transaction.
/// So no pre-revoke yes can be picked up by `cards::raise` after an undo: undo restores access, never these cards.
/// The ids are those of `follow_in_doors` (never the owner, whose cards carry an empty person). The withdrawn card ids
/// go into the answer under `cards`, for the caller to tell a waiting asker.
pub(super) fn follow_in_cards(
    conn: &rusqlite::Connection,
    method: &str,
    params: &Value,
    said: &mut Value,
) -> crate::error::Result<()> {
    let ids: Vec<String> = match method {
        "people.suspend" | "people.removePerson" if said.get("ids").is_some() => said["ids"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|x| x.as_str().map(str::to_owned))
            .collect(),
        "people.removeBot" => params
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .into_iter()
            .collect(),
        "people.renamePerson" => {
            if let (Some(old), Some(new)) = (
                params.get("id").and_then(Value::as_str),
                params.get("to").and_then(Value::as_str),
            ) {
                crate::cards::renamed(conn, old, new)?;
            }
            return Ok(());
        }
        _ => return Ok(()),
    };
    // a removal has already dropped the person's row, so the write passes their old ids along (`was_ids`)
    let mut ids = ids;
    if let Some(Value::Array(was)) = said.as_object_mut().and_then(|o| o.remove("was")) {
        ids.extend(was.iter().filter_map(|x| x.as_str().map(str::to_owned)));
    }
    let gone = crate::cards::withdraw_people(conn, &ids)?;
    said["cards"] = Value::Array(gone.into_iter().map(Value::String).collect());
    Ok(())
}

/// The old ids (`was`) of a person and of their bots, for [`follow_in_cards`]: read before a removal drops the rows.
pub(super) fn was_ids(auth: &Value, pid: &str) -> Vec<String> {
    let canon = secrets::canonical(auth, pid);
    std::iter::once(canon.clone())
        .chain(bots_of(auth, &canon))
        .flat_map(|x| words(people(auth).get(&x).and_then(|p| p.get("was"))))
        .collect()
}

/// `person <id> --clear-temp`: the ids whose temp folders go (the person and their bots).
pub(crate) fn temp_ids(auth: &Value, pid: &str) -> R<Vec<String>> {
    check_id(pid)?;
    let mut ids = vec![pid.to_owned()];
    ids.extend(bots_of(auth, &secrets::canonical(auth, pid)));
    Ok(ids)
}

/// `guest_tmp_remove`: the temp folder of a person's runs, `<machine>/run/t/<id>` (or `<8 hex of sha256(id)>` when the id
/// is too long for Claude Code's sockets), once their runs are stopped: the record of whose it is first, so whatever a
/// late write left is emptied before their next run. A folder recorded as someone else's stays. A folder under the
/// hash name with no record is left alone: whose it is is not known (kith-auth removes it; the next run empties it).
pub(super) fn guest_tmp_remove(life: &Life, pid: &str) {
    let run = life.machine.join("run");
    let digest = Sha256::digest(pid.as_bytes());
    let mut hash = String::new();
    for b in digest.iter().take(4) {
        let _ = write!(hash, "{b:02x}");
    }
    for (name, needs_record) in [(pid.to_owned(), false), (hash, true)] {
        let rec = run.join("t-who").join(&name);
        let theirs = match std::fs::read_to_string(&rec) {
            Ok(who) => who == pid,
            Err(_) => !needs_record,
        };
        if !theirs {
            continue;
        }
        let _ = std::fs::remove_file(&rec);
        let d = run.join("t").join(&name);
        // a link inside is removed, never what it names
        match d.symlink_metadata() {
            Ok(m) if m.is_dir() => {
                let _ = std::fs::remove_dir_all(&d);
            }
            Ok(_) => {
                let _ = std::fs::remove_file(&d);
            }
            Err(_) => {}
        }
    }
}
