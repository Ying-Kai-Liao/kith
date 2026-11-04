//! Every account, as `kith account list` shows them: `known_ids`, `rows` and the table of `kernel/bin/kith-account`'s
//! `cmd_list`. The rows are the contract of `kith account list --json` (docs/accounts.md).

use super::pick::{Status, pick, statuses};
use super::state::{self, Mark};
use super::{
    Ctx, Map, account_dir, agent_harness, agent_md_of, clock, kind, parse, pyjson, settings,
    static_order,
};
use serde_json::Value;
use std::collections::HashMap;
use std::fmt::Write as _;

/// Every id worth listing: the sets (global, each agent's, or just this agent's), pins, the default logins of the installed
/// harnesses, and every account folder on this Mac. Hand-edited bad ids stay in, as invalid.
fn known_ids(ctx: &Ctx<'_>, s: &Map, agent: &str) -> Vec<String> {
    let mut ids: Vec<String> = s
        .get("accounts")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|i| i.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    if let Some(agents) = s.get("agents").and_then(Value::as_object) {
        for (name, a) in agents {
            if !agent.is_empty() && name != agent {
                continue;
            }
            let Some(a) = a.as_object() else { continue };
            ids.extend(
                a.get("accounts")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|i| i.as_str().map(str::to_owned)),
            );
            if let Some(pin) = a
                .get("pin")
                .and_then(|p| p.get("id"))
                .and_then(Value::as_str)
            {
                ids.push(pin.to_owned());
            }
        }
    }
    for h in ["claude", "codex"] {
        if ctx.out.on_path(h) {
            ids.push(h.to_owned());
        }
        let root = ctx.machine.join("accounts").join(h);
        let mut dirs: Vec<_> = std::fs::read_dir(&root)
            .map(|rd| rd.filter_map(Result::ok).map(|e| e.path()).collect())
            .unwrap_or_default();
        dirs.sort();
        for d in dirs {
            let name = d
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            if d.is_dir() && !name.starts_with('.') {
                ids.push(format!("{h}:{name}"));
            }
        }
    }
    dedup(ids)
}

fn dedup(ids: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for i in ids {
        if !out.contains(&i) {
            out.push(i);
        }
    }
    out
}

/// A usage reading as the row shows it (`usage_json`), `null` when there is none or it is damaged.
fn usage_json(e: Option<&Value>, now: f64) -> Value {
    let reading = || -> Option<Value> {
        let u = e?.get("usage")?.as_object().filter(|u| !u.is_empty())?;
        let read_at = u.get("read_at")?;
        let at = read_at.as_f64()?;
        let mut wins = vec![];
        for w in u.get("windows")?.as_array()? {
            let mut m = Map::new();
            m.insert("name".to_owned(), w.get("name")?.clone());
            m.insert("percent".to_owned(), w.get("percent")?.clone());
            if let Some(r) = w.get("resets_at") {
                m.insert("resets_at".to_owned(), r.clone());
            }
            wins.push(Value::Object(m));
        }
        let mut m = Map::new();
        #[allow(clippy::cast_possible_truncation)]
        {
            m.insert("read_at".to_owned(), Value::from(at as i64));
            m.insert("age_s".to_owned(), Value::from((now - at).max(0.0) as i64));
        }
        m.insert("windows".to_owned(), Value::Array(wins));
        Some(Value::Object(m))
    };
    reading().unwrap_or(Value::Null)
}

fn opt(v: Option<&str>) -> Value {
    v.map_or(Value::Null, |s| Value::String(s.to_owned()))
}

/// One row per account (the contract of `kith account list --json`, an array of); with an agent, `in_set`/`order` are that
/// agent's static order (pin first) and `candidate` is its place in `pick`.
pub fn rows(ctx: &Ctx<'_>, s: &Map, agent: &str, deep: bool, now: f64) -> Vec<Map> {
    rows_from(ctx, s, agent, deep, now, &state::FileState)
}

/// [`rows`] over a state source (the file by default, the core's database in the core).
pub fn rows_from(
    ctx: &Ctx<'_>,
    s: &Map,
    agent: &str,
    deep: bool,
    now: f64,
    src: &dyn state::StateSource,
) -> Vec<Map> {
    let state = src.accounts(&ctx.machine);
    let out: HashMap<String, Mark> = state::out_marks(&ctx.machine, &state, now);
    let (in_order, cands): (Vec<String>, Vec<String>) = if agent.is_empty() {
        (settings::accounts(s, "").0.unwrap_or_default(), vec![])
    } else {
        let harness = agent_harness(agent_md_of(ctx, agent).as_deref());
        let (order, _) = static_order(s, agent, &harness);
        let p = pick(ctx, s, agent, None, deep, now, &state);
        (order, p.candidates)
    };
    let main = if agent.is_empty() {
        None
    } else {
        in_order.first().cloned()
    };
    let ids = dedup(
        in_order
            .iter()
            .cloned()
            .chain(known_ids(ctx, s, agent))
            .collect(),
    );
    let status: HashMap<String, Status> =
        statuses(ctx, &ids, agent, &out, deep, false, main.as_deref());
    let mut pins: HashMap<String, Vec<Value>> = HashMap::new();
    if let Some(agents) = s.get("agents").and_then(Value::as_object) {
        for (name, a) in agents {
            if let Some(pin) = a
                .get("pin")
                .and_then(Value::as_object)
                .filter(|p| !p.is_empty())
            {
                let mut m = Map::new();
                m.insert("agent".to_owned(), Value::String(name.clone()));
                m.insert(
                    "only".to_owned(),
                    Value::Bool(pin.get("only").and_then(Value::as_bool).unwrap_or(false)),
                );
                pins.entry(
                    pin.get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                )
                .or_default()
                .push(Value::Object(m));
            }
        }
    }
    let mut result = vec![];
    for i in &ids {
        let parsed = parse(i).ok();
        let (harness, name) = match &parsed {
            Some((h, n)) => (Some(h.as_str()), n.as_deref()),
            None => (None, None),
        };
        let st = &status[i];
        let e = state.get(i);
        let place = in_order.iter().position(|x| x == i);
        let mut row = Map::new();
        let put = |row: &mut Map, k: &str, v: Value| {
            row.insert(k.to_owned(), v);
        };
        put(&mut row, "id", Value::String(i.clone()));
        put(
            &mut row,
            "harness",
            opt(harness.map(|h| if h == "claude-api" { "claude" } else { h })),
        );
        put(&mut row, "account", opt(name));
        put(
            &mut row,
            "kind",
            Value::String(harness.map_or("unknown", kind).to_owned()),
        );
        put(&mut row, "in_set", Value::Bool(place.is_some()));
        put(
            &mut row,
            "order",
            place.map_or(Value::Null, |n| Value::from(n + 1)),
        );
        put(&mut row, "status", Value::String(st.status.to_owned()));
        put(&mut row, "ready", Value::Bool(st.status == "ready"));
        put(&mut row, "why", opt(st.why.as_deref()));
        put(&mut row, "fix", opt(st.fix.as_deref()));
        put(&mut row, "usage", usage_json(e, now));
        put(
            &mut row,
            "last_started",
            e.and_then(|e| e.get("last_started"))
                .filter(|v| v.is_number())
                .cloned()
                .unwrap_or(Value::Null),
        );
        put(
            &mut row,
            "pins",
            Value::Array(pins.remove(i).unwrap_or_default()),
        );
        if st.status == "out" {
            put(
                &mut row,
                "out_until",
                Value::from(st.out_until.unwrap_or(0)),
            );
            put(&mut row, "reason", opt(st.reason.as_deref()));
        }
        if !agent.is_empty() {
            put(
                &mut row,
                "candidate",
                cands
                    .iter()
                    .position(|x| x == i)
                    .map_or(Value::Null, |n| Value::from(n + 1)),
            );
        }
        // the older fields, which the host reads
        put(
            &mut row,
            "logged_in",
            Value::Bool(st.status != "signed_out"),
        );
        if let (Some(h @ ("claude" | "codex")), Some(n)) = (harness, name) {
            put(&mut row, "name", Value::String(n.to_owned()));
            put(
                &mut row,
                "dir",
                Value::String(account_dir(ctx, h, n).to_string_lossy().into_owned()),
            );
        }
        result.push(row);
    }
    result
}

fn status_text(r: &Map) -> String {
    let get = |k: &str| r.get(k).and_then(Value::as_str).unwrap_or_default();
    match get("status") {
        "out" => format!(
            "out until {} ({})",
            clock::when(r.get("out_until").and_then(Value::as_i64).unwrap_or(0)),
            get("reason")
        ),
        "ready" => "ready".to_owned(),
        "signed_out" => "signed out".to_owned(),
        other => {
            let word = if other == "unsupported" {
                "not supported yet"
            } else {
                "invalid id"
            };
            match r
                .get("why")
                .and_then(Value::as_str)
                .filter(|w| !w.is_empty())
            {
                Some(w) => format!("{word} ({w})"),
                None => word.to_owned(),
            }
        }
    }
}

/// Python's `format(x, 'g')`: six significant digits, no trailing zeros.
fn fmt_g(x: f64) -> String {
    if x == 0.0 {
        return "0".to_owned();
    }
    let e = format!("{x:.5e}");
    let (mantissa, exp) = e.split_once('e').unwrap_or((&e, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let trim = |m: &str| {
        if m.contains('.') {
            m.trim_end_matches('0').trim_end_matches('.').to_owned()
        } else {
            m.to_owned()
        }
    };
    if (-4..6).contains(&exp) {
        let decimals = usize::try_from(5 - exp).unwrap_or(0);
        trim(&format!("{x:.decimals$}"))
    } else {
        format!(
            "{}e{}{:02}",
            trim(mantissa),
            if exp < 0 { '-' } else { '+' },
            exp.abs()
        )
    }
}

fn ago(seconds: i64) -> String {
    for (unit, size) in [("d", 86_400), ("h", 3600), ("m", 60)] {
        if seconds >= size {
            return format!("{}{unit}", seconds / size);
        }
    }
    if seconds < 5 {
        "now".to_owned()
    } else {
        format!("{seconds}s")
    }
}

fn usage_text(r: &Map) -> String {
    let Some(u) = r.get("usage").and_then(Value::as_object) else {
        return String::new();
    };
    let wins: Vec<String> = u
        .get("windows")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|w| {
            let name = w.get("name").and_then(Value::as_str).unwrap_or_default();
            let name = match name {
                "five_hour" => "5h",
                "seven_day" | "weekly" => "week",
                n => n,
            };
            format!(
                "{name} {}%",
                fmt_g(w.get("percent").and_then(Value::as_f64).unwrap_or(0.0))
            )
        })
        .collect();
    format!(
        "{} ({} ago)",
        wins.join(" "),
        ago(u.get("age_s").and_then(Value::as_i64).unwrap_or(0))
    )
}

/// `kith account list [--agent A] [--json]`: every account, whether it is in the set (and its place), ready / out until a
/// time / signed out (and the command that fixes it), last usage, pins.
pub fn list(ctx: &Ctx<'_>, json: bool, agent: &str) -> String {
    let s = settings::read(ctx);
    let rows = rows(ctx, &s, agent, true, clock::now());
    if json {
        return pyjson::dumps(&Value::Array(rows.into_iter().map(Value::Object).collect())) + "\n";
    }
    if rows.is_empty() {
        return "no accounts (kith account add claude|codex <name>)\n".to_owned();
    }
    let text = |r: &Map, k: &str| {
        r.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let mut table = vec![
        ["ID", "SET", "STATUS", "USAGE", "PINNED"]
            .map(str::to_owned)
            .to_vec(),
    ];
    for r in &rows {
        let in_set = r.get("in_set").and_then(Value::as_bool).unwrap_or(false);
        let mut place = if in_set {
            r.get("order")
                .and_then(Value::as_i64)
                .map_or_else(String::new, |n| n.to_string())
        } else {
            "-".to_owned()
        };
        if let Some(c) = r
            .get("candidate")
            .and_then(Value::as_i64)
            .filter(|c| *c != 0)
        {
            place = format!("{place}  starts #{c}");
        }
        let pins: Vec<String> = r
            .get("pins")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|p| {
                let only = p.get("only").and_then(Value::as_bool).unwrap_or(false);
                format!(
                    "{}{}",
                    p.get("agent").and_then(Value::as_str).unwrap_or_default(),
                    if only { " (only)" } else { "" }
                )
            })
            .collect();
        table.push(vec![
            text(r, "id"),
            place,
            status_text(r),
            usage_text(r),
            pins.join(", "),
        ]);
    }
    let widths: Vec<usize> = (0..4)
        .map(|i| {
            table
                .iter()
                .map(|t| t[i].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    let mut out = String::new();
    for t in &table {
        let cells: Vec<String> = t
            .iter()
            .enumerate()
            .map(|(i, c)| match widths.get(i) {
                Some(w) => format!("{c:<w$}"),
                None => c.clone(),
            })
            .collect();
        out.push_str(cells.join("  ").trim_end());
        out.push('\n');
    }
    for r in &rows {
        if text(r, "status") == "signed_out"
            && let Some(fix) = r
                .get("fix")
                .and_then(Value::as_str)
                .filter(|f| !f.is_empty())
        {
            let _ = writeln!(
                out,
                "  {}: {}; to sign it in: {fix}",
                text(r, "id"),
                text(r, "why")
            );
        }
    }
    let cross = settings::cross_harness(&s, agent);
    let _ = writeln!(
        out,
        "policy: {}; cross-harness fallback: {}{}",
        settings::policy(&s),
        if cross { "on" } else { "off" },
        if agent.is_empty() {
            String::new()
        } else {
            format!(" (for {agent})")
        }
    );
    out
}
