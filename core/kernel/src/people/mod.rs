//! `kith kernel people people|invite ...`: the command line of `kernel/bin/kith-people` in Rust. `people [ls]` and
//! `people show` read who is invited from `kith-auth people --json` (the one reader of auth.json). The switch point is
//! `kith-auth people --json` itself, which needs a core read verb (`id`, `name`, `owner`, `bot_of`, `root`, `revoked` / `cut_off`,
//! `revoked_at` and the agent rows) rather than `Backend::people`; the M2 lane owns that verb. `invite add|ls|revoke` go
//! through the running host's loopback API with the owner token (`system::host_call`, `kith_host.host_call`'s twin). Same
//! stdout, stderr and exit codes (0 done, 1 refused / host unreachable / not found, 2 usage); the difftest golden
//! `people.json` holds the Python's answers. `kith-people` stays Python until `people` is in `lib/core-switched.txt`.
//!
//! Never prints a token or hash: the host's invite list is scrubbed of `tokenHash`, `token_hash`, `token` and `proof` at
//! any depth, and `invite add` prints only the app's own fields (the link is the one thing that carries a token, for the
//! owner's terminal).
//!
//! Where the Python crashes with a traceback (an unknown first word, an auth row with no `via`), this answers a short line
//! and exit 1.

mod system;
#[cfg(test)]
mod tests;

use crate::secrets::pyjson;
use serde_json::{Map as JsonMap, Value, json};

pub use system::{Env, System};

type Map = JsonMap<String, Value>;

/// `__doc__`.
const USAGE: &str = include_str!("usage.txt");
const ROLES: [&str; 2] = ["member", "guest"];
const SECRET_KEYS: [&str; 4] = ["tokenHash", "token_hash", "token", "proof"];

/// What a run prints and exits with.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Out {
    pub stdout: String,
    pub stderr: String,
    pub code: i32,
}

/// Where the data comes from: kith-auth for people, the host for invites. A test stands in for both.
pub trait Backend {
    /// `kith-auth people --json`, parsed.
    fn people(&self) -> Result<Value, String>;
    /// `kith_host.host_call`: the answer, or why the host could not be reached or refused.
    fn host_call(&self, method: &str, body: &Value) -> Result<Value, String>;
}

struct Fail {
    msg: String,
    code: i32,
}

fn fail<T>(msg: impl Into<String>, code: i32) -> Result<T, Fail> {
    Err(Fail {
        msg: msg.into(),
        code,
    })
}

impl From<String> for Fail {
    fn from(msg: String) -> Fail {
        Fail { msg, code: 1 }
    }
}

type R<T> = Result<T, Fail>;

/// `main()`: `args` is the line after the program name, `people ...` or `invite ...`.
pub fn run(b: &dyn Backend, args: &[String]) -> Out {
    let help = |a: Option<&String>| matches!(a.map(String::as_str), Some("-h" | "--help"));
    let Some(first) = args.first() else {
        return Out {
            stdout: format!("{USAGE}\n"),
            code: 2,
            ..Out::default()
        };
    };
    if help(Some(first)) || help(args.get(1)) {
        return Out {
            stdout: format!("{USAGE}\n"),
            ..Out::default()
        };
    }
    let json_mode = args.iter().any(|a| a == "--json");
    let res = match first.as_str() {
        "people" => cmd_people(b, &args[1..]),
        "invite" => cmd_invite(b, &args[1..]),
        other => fail(format!("no such command '{other}' (people, invite)"), 1),
    };
    match res {
        Ok(stdout) => Out {
            stdout,
            ..Out::default()
        },
        Err(Fail { msg, code }) if json_mode => Out {
            stdout: format!("{}\n", pyjson::dumps(&json!({"ok": false, "error": msg}))),
            code,
            ..Out::default()
        },
        Err(Fail { msg, code }) => Out {
            stderr: format!("kith: {msg}\n"),
            code,
            ..Out::default()
        },
    }
}

fn out(as_json: bool, obj: &Value, text: &str) -> String {
    if as_json {
        format!("{}\n", pyjson::dumps(obj))
    } else {
        format!("{text}\n")
    }
}

// --- Python's values ------------------------------------------------------------------------------------------------

/// `bool(v)`.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `str(v)`, what an f-string prints.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => py_repr(other),
    }
}

fn py_repr(v: &Value) -> String {
    match v {
        Value::Null => "None".to_owned(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        Value::Number(_) => pyjson::dumps(v),
        Value::String(s) => {
            let q = if s.contains('\'') && !s.contains('"') {
                '"'
            } else {
                '\''
            };
            let body: String = s
                .chars()
                .flat_map(|c| match c {
                    '\\' => vec!['\\', '\\'],
                    '\n' => vec!['\\', 'n'],
                    '\r' => vec!['\\', 'r'],
                    '\t' => vec!['\\', 't'],
                    c if c == q => vec!['\\', c],
                    c => vec![c],
                })
                .collect();
            format!("{q}{body}{q}")
        }
        Value::Array(a) => format!("[{}]", a.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter()
                .map(|(k, x)| format!("{}: {}", py_repr(&Value::String(k.clone())), py_repr(x)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

static NULL: Value = Value::Null;

fn get<'a>(v: &'a Value, k: &str) -> &'a Value {
    v.get(k).unwrap_or(&NULL)
}

fn array(v: &Value) -> &[Value] {
    v.as_array().map_or(&[], Vec::as_slice)
}

/// Whatever the host sends, a hash or token field never goes further.
fn scrub(v: &Value) -> Value {
    match v {
        Value::Object(o) => Value::Object(
            o.iter()
                .filter(|(k, _)| !SECRET_KEYS.contains(&k.as_str()))
                .map(|(k, x)| (k.clone(), scrub(x)))
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(scrub).collect()),
        other => other.clone(),
    }
}

// --- the command line -----------------------------------------------------------------------------------------------

struct Flags {
    pos: Vec<String>,
    one: Map,
    many: Vec<(String, Vec<String>)>,
    json: bool,
}

impl Flags {
    fn many(&self, flag: &str) -> &[String] {
        self.many
            .iter()
            .find(|(k, _)| k == flag)
            .map_or(&[], |(_, v)| v.as_slice())
    }
}

/// A flag in neither list is a usage error.
fn split_flags(args: &[String], valued: &[&str], repeated: &[&str]) -> R<Flags> {
    let mut f = Flags {
        pos: Vec::new(),
        one: Map::new(),
        many: repeated
            .iter()
            .map(|k| ((*k).to_owned(), Vec::new()))
            .collect(),
        json: false,
    };
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--json" {
            f.json = true;
        } else if valued.contains(&a) || repeated.contains(&a) {
            if i + 1 >= args.len() || args[i + 1].starts_with("--") {
                return fail(format!("{a} needs a value"), 2);
            }
            i += 1;
            if valued.contains(&a) {
                f.one.insert(a.to_owned(), Value::String(args[i].clone()));
            } else if let Some((_, v)) = f.many.iter_mut().find(|(k, _)| k == a) {
                v.push(args[i].clone());
            }
        } else if a.starts_with("--") {
            return fail(format!("unknown option {a}"), 2);
        } else {
            f.pos.push(a.to_owned());
        }
        i += 1;
    }
    Ok(f)
}

// --- people ---------------------------------------------------------------------------------------------------------

/// An invitee as the host and apps read them. Bots are nested under their owner, with their own agents.
fn view(p: &Value, bots: &[&Value]) -> R<Value> {
    let agents = |via: &str| -> R<Vec<Value>> {
        let mut rows = Vec::new();
        for a in array(get(p, "agents")) {
            // The Python reads `a['via']` on every row, but `agent` and `role` only on the rows it keeps.
            let Some(v) = a.get("via") else {
                return fail("kith-auth people gave an agent with no via", 1);
            };
            if v.as_str() != Some(via) {
                continue;
            }
            let (Some(agent), Some(role)) = (a.get("agent"), a.get("role")) else {
                return fail("kith-auth people gave an agent with no agent or role", 1);
            };
            rows.push(json!({"agent": agent, "role": role}));
        }
        Ok(rows)
    };
    let mut nested = Vec::new();
    for b in bots {
        let Some(id) = b.get("id") else {
            return fail("kith-auth people gave a bot with no id", 1);
        };
        nested.push(json!({
            "id": id,
            "name": b.get("name").cloned().unwrap_or_else(|| json!("")),
            "revoked": truthy(get(b, "revoked")),
            "agents": array(get(b, "agents")).iter().map(|a| get(a, "agent").clone()).collect::<Vec<_>>(),
        }));
    }
    let Some(id) = p.get("id") else {
        return fail("kith-auth people gave a person with no id", 1);
    };
    Ok(json!({
        "id": id,
        "name": p.get("name").cloned().unwrap_or_else(|| json!("")),
        "revoked": truthy(get(p, "revoked")),
        "revoked_at": get(p, "revoked_at"),
        "agents": agents("own")?,
        "everyone_agents": agents("everyone")?,
        "bots": nested,
    }))
}

fn invitees(b: &dyn Backend) -> R<Vec<Value>> {
    let rows = b.people()?;
    let rows = array(&rows);
    let mut out = Vec::new();
    for p in rows {
        if truthy(get(p, "owner")) || truthy(get(p, "bot_of")) {
            continue;
        }
        let bots: Vec<&Value> = rows
            .iter()
            .filter(|x| x.get("bot_of").is_some_and(|o| Some(o) == p.get("id")))
            .collect();
        out.push(view(p, &bots)?);
    }
    Ok(out)
}

fn describe(v: &Value) -> String {
    let state = if truthy(get(v, "revoked")) {
        " (access removed)"
    } else {
        ""
    };
    let agents: Vec<String> = array(get(v, "agents"))
        .iter()
        .map(|a| format!("{}={}", py_str(get(a, "agent")), py_str(get(a, "role"))))
        .collect();
    let agents = if agents.is_empty() {
        "no agents".to_owned()
    } else {
        agents.join(", ")
    };
    let mut line = format!(
        "{}{state}  {}  ·  {agents}",
        py_str(get(v, "id")),
        py_str(get(v, "name"))
    );
    let bots = array(get(v, "bots"));
    if !bots.is_empty() {
        let names: Vec<String> = bots
            .iter()
            .map(|b| {
                format!(
                    "{}{}",
                    py_str(get(b, "id")),
                    if truthy(get(b, "revoked")) {
                        " (removed)"
                    } else {
                        ""
                    }
                )
            })
            .collect();
        line.push_str("  ·  bots: ");
        line.push_str(&names.join(", "));
    }
    line
}

fn cmd_people(b: &dyn Backend, args: &[String]) -> R<String> {
    let named = args.first().is_some_and(|a| !a.starts_with("--"));
    let verb = if named { args[0].as_str() } else { "ls" };
    let rest = if named { &args[1..] } else { args };
    let f = split_flags(rest, &[], &[])?;
    match (verb, f.pos.as_slice()) {
        ("ls", []) => {
            let people = invitees(b)?;
            let text = people.iter().map(describe).collect::<Vec<_>>().join("\n");
            let text = if text.is_empty() {
                "nobody invited yet (kith invite add)".to_owned()
            } else {
                text
            };
            Ok(out(f.json, &json!({"ok": true, "people": people}), &text))
        }
        ("show", [id]) => {
            let found = invitees(b)?
                .into_iter()
                .find(|v| v.get("id").and_then(Value::as_str) == Some(id.as_str()));
            let Some(found) = found else {
                return fail(format!("no invited person '{id}' (kith people ls)"), 1);
            };
            let text = describe(&found);
            Ok(out(f.json, &json!({"ok": true, "person": found}), &text))
        }
        _ => fail(
            "usage: kith people [ls] [--json] | kith people show <id> [--json]",
            2,
        ),
    }
}

// --- invites --------------------------------------------------------------------------------------------------------

/// A name `--agent` accepts: `[a-z0-9][a-z0-9_-]*`.
fn agent_name(s: &str) -> bool {
    let mut cs = s.chars();
    cs.next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && cs.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

fn cmd_invite(b: &dyn Backend, args: &[String]) -> R<String> {
    let (verb, rest) = args
        .split_first()
        .map_or(("", &[][..]), |(v, r)| (v.as_str(), r));
    match verb {
        "add" => invite_add(b, rest),
        "ls" => {
            let f = split_flags(rest, &[], &[])?;
            if !f.pos.is_empty() {
                return fail("usage: kith invite ls [--json]", 2);
            }
            let ans = b.host_call("invites", &json!({}))?;
            let invs = match scrub(get(&ans, "invites")) {
                Value::Array(a) => a,
                _ => Vec::new(),
            };
            let state = |i: &Value| {
                if truthy(get(i, "usedAt")) {
                    "used"
                } else if truthy(get(i, "revokedAt")) {
                    "revoked"
                } else {
                    "open"
                }
            };
            let lines: Vec<String> = invs
                .iter()
                .map(|i| {
                    let agents = get(i, "agents").as_object().map_or_else(String::new, |o| {
                        o.iter()
                            .map(|(a, r)| format!("{a}={}", py_str(r)))
                            .collect::<Vec<_>>()
                            .join(", ")
                    });
                    format!(
                        "{}  {}  {}  {agents}",
                        py_str(get(i, "inviteId")),
                        py_str(get(i, "personId")),
                        state(i)
                    )
                })
                .collect();
            let text = if lines.is_empty() {
                "no invites".to_owned()
            } else {
                lines.join("\n")
            };
            Ok(out(f.json, &json!({"ok": true, "invites": invs}), &text))
        }
        "revoke" => {
            let f = split_flags(rest, &[], &[])?;
            let [id] = f.pos.as_slice() else {
                return fail("usage: kith invite revoke <inviteId> [--json]", 2);
            };
            let ans = b.host_call("revokeInvite", &json!({"inviteId": id}))?;
            let changed = truthy(get(&ans, "revoked"));
            let text = if changed {
                format!("{id} revoked")
            } else {
                format!("{id} was already used, revoked or gone")
            };
            Ok(out(
                f.json,
                &json!({"ok": true, "inviteId": id, "changed": changed}),
                &text,
            ))
        }
        _ => fail("usage: kith invite add|ls|revoke (kith invite --help)", 2),
    }
}

fn invite_add(b: &dyn Backend, rest: &[String]) -> R<String> {
    let f = split_flags(rest, &["--person"], &["--agent"])?;
    let name = f.pos.first().cloned().unwrap_or_default();
    let person = f
        .one
        .get("--person")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    if f.pos.len() > 1 || (name.is_empty() && person.is_empty()) {
        return fail(
            "usage: kith invite add <name> --agent A[=member|guest] ... [--person ID] [--json]",
            2,
        );
    }
    let mut agents = Map::new();
    for spec in f.many("--agent") {
        let (agent, role) = spec.split_once('=').unwrap_or((spec.as_str(), ""));
        let role = if role.is_empty() { "guest" } else { role };
        if !agent_name(agent) || !ROLES.contains(&role) {
            return fail(
                format!("--agent {spec}: an agent name, then = member or guest"),
                2,
            );
        }
        agents.insert(agent.to_owned(), Value::String(role.to_owned()));
    }
    if agents.is_empty() {
        return fail(
            "say what they get with --agent <agent>[=member|guest] (repeat for more)",
            2,
        );
    }
    let mut body = Map::new();
    if person.is_empty() {
        body.insert("name".to_owned(), Value::String(name));
    } else {
        body.insert("personId".to_owned(), Value::String(person));
    }
    body.insert("agents".to_owned(), Value::Object(agents));
    let ans = b.host_call("createInvite", &Value::Object(body))?;
    // The app's own fields, no more. The link is the one thing that carries the token, for the owner's terminal only.
    let res = json!({
        "ok": true, "inviteId": get(&ans, "inviteId"), "personId": get(&ans, "personId"),
        "link": get(&ans, "link"), "expiresAt": get(&ans, "expiresAt"),
    });
    let text = format!(
        "invited {}. Send them this link (it works once):\n{}",
        py_str(get(&res, "personId")),
        py_str(get(&res, "link"))
    );
    Ok(out(f.json, &res, &text))
}
