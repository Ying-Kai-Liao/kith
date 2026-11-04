//! Delegation limits: how deep a chain of `kith ask`s may go and what it may cost. A port of the caps and the record
//! reading in `kernel/bin/kith-ask` (`upgrade`, `read_reg`, `records`, `chain_costs`, `lineage`, the checks in
//! `open_record`), which stays the source of truth while the verb is not switched. The host's bot requests and its
//! delegations API take their numbers from here.
//!
//! This side only decides and reads; it writes no record. `kith-ask` is still the one writer of `live/ask/*.reg`
//! (switching the writer is a later step), so nothing here can race it. Like the Python, a check takes the box lock
//! (`live/ask/.lock`) while it reads the records, so its answer is one the writer could have seen.

use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::path::{Path, PathBuf};

use fs2::FileExt as _;
use serde_json::{Map, Value, json};

use crate::accounts::read_settings;

/// Asks deep in a chain on one Mac (as the host's bot requests: `serve/host/src/chat/team/requests.rs`).
pub const MAX_LOCAL_HOPS: u8 = 8;
/// Asks in a chain that cross to another Mac.
pub const MAX_CROSS_HOPS: u8 = 2;
/// The exit code of an ask a cap refused: not the agent failing, and not "still waiting" (3).
pub const REFUSED: i32 = 4;
/// The cost cap of a chain, US$, when `settings.json` does not say.
pub const DEFAULT_CAP_USD: f64 = 5.0;

/// Where an ask's files live: `live/ask` in the Mac's folder.
pub fn box_dir(machine: &Path) -> PathBuf {
    machine.join("live").join("ask")
}

/// Whether the ask's run waits on a card (`ID.parked` beside its answers). `id` is a safe kernel id.
pub fn parked(machine: &Path, id: &str) -> bool {
    box_dir(machine).join(format!("{id}.parked")).exists()
}

/// An ask id as the kernel makes them: `[A-Za-z0-9._-]{1,64}`.
pub fn safe_id(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// An ask a cap refused: the words `kith ask` prints (after `kith ask: `) and exits `REFUSED` with.
#[derive(Debug, PartialEq, Eq)]
pub struct Refusal(pub String);

impl Refusal {
    fn new(why: impl Into<String>) -> Self {
        Self(why.into())
    }
}

// --- the record -------------------------------------------------------------------------------------------------------

/// A delegation record as the code reads it: the JSON object of a `.reg` file, or the same shape built from a core.db row.
pub type Reg = Map<String, Value>;

/// Python's truthiness of a JSON value.
pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// A delegation record as the code reads it (`upgrade` in kith-ask). A registration made before the record existed has
/// no chain, hop or cost: it reads as a root ask at hop 0 with its cost unknown. Every other field is kept as it was.
pub fn upgrade(id: &str, raw: &Reg) -> Reg {
    let mut reg = raw.clone();
    let session = reg.remove("session"); // the caller's session now lives in reply_to, and only there
    if !reg.get("reply_to").is_some_and(Value::is_object) {
        reg.insert(
            "reply_to".into(),
            json!({"session": session.unwrap_or(Value::Null), "ask": null}),
        );
    }
    reg.entry("chain").or_insert_with(|| json!(id));
    for key in ["hop", "cross_hops"] {
        reg.entry(key).or_insert_with(|| json!(0));
    }
    reg.entry("crossed").or_insert(json!(false));
    reg.entry("cost").or_insert(Value::Null);
    if !reg.contains_key("state") {
        let state = if truthy(reg.get("done")) {
            "answered"
        } else {
            "open"
        };
        reg.insert("state".into(), json!(state));
    }
    reg
}

/// The record of ask `id`, upgraded; `None` when it has none or the file is damaged (unreadable, not JSON, not an object).
pub fn read_reg(box_dir: &Path, id: &str) -> Option<Reg> {
    let text = std::fs::read_to_string(box_dir.join(format!("{id}.reg"))).ok()?;
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(raw)) => Some(upgrade(id, &raw)),
        _ => None,
    }
}

/// `{ask id: record}` of every delegation record on this Mac, in id order. A damaged record is left out.
pub fn records(box_dir: &Path) -> BTreeMap<String, Reg> {
    let mut out = BTreeMap::new();
    let Ok(dir) = std::fs::read_dir(box_dir) else {
        return out;
    };
    for entry in dir.filter_map(Result::ok) {
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(id) = name.strip_suffix(".reg")
            && let Some(reg) = read_reg(box_dir, id)
        {
            out.insert(id.to_owned(), reg);
        }
    }
    out
}

fn int_of(reg: &Reg, key: &str) -> i64 {
    reg.get(key).and_then(Value::as_i64).unwrap_or(0)
}

fn num_of(reg: &Reg, key: &str) -> f64 {
    reg.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

fn chain_of(reg: &Reg) -> Option<&str> {
    reg.get("chain").and_then(Value::as_str)
}

/// `{chain: US$ it has cost}`: what a Mac it crossed from had spent when it came here (the most any record says), plus
/// the runs of its records here (a record from before costs were kept counts nothing).
pub fn chain_costs(recs: &BTreeMap<String, Reg>) -> HashMap<String, f64> {
    let mut base: HashMap<String, f64> = HashMap::new();
    let mut total: HashMap<String, f64> = HashMap::new();
    for r in recs.values() {
        let chain = chain_of(r).unwrap_or_default().to_owned();
        let b = base.entry(chain.clone()).or_insert(0.0);
        *b = b.max(num_of(r, "cost_base"));
        *total.entry(chain).or_insert(0.0) += num_of(r, "cost");
    }
    base.into_iter()
        .map(|(c, b)| {
            let t = total.get(&c).copied().unwrap_or(0.0);
            (c, b + t)
        })
        .collect()
}

// --- the cap ----------------------------------------------------------------------------------------------------------

/// The dollars one chain may cost before it starts no new ask, `None` for no cap. A settings file that can't be read
/// leaves the default, so a broken file never lifts the cap (`kith_settings.chain_cost_cap`).
pub fn chain_cost_cap(machine: &Path) -> Option<f64> {
    let s = read_settings(machine);
    match s
        .get("delegation")
        .and_then(|d| d.get("chain_cost_cap_usd"))
    {
        None => Some(DEFAULT_CAP_USD),
        Some(Value::String(_)) => None, // "none": anything else was refused by the reader
        Some(v) => v.as_f64().or(Some(DEFAULT_CAP_USD)),
    }
}

/// Python's `'%g' % x` for the caps a chain can have (above 0, below 1e7): six significant digits.
pub fn pct_g(x: f64) -> String {
    if x == 0.0 {
        return "0".into();
    }
    let sci = format!("{x:.5e}");
    let (mant, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let strip = |s: &str| -> String {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_owned()
        } else {
            s.to_owned()
        }
    };
    if (-4..6).contains(&exp) {
        let decimals = usize::try_from(5 - exp).unwrap_or(0);
        strip(&format!("{x:.decimals$}"))
    } else {
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{}e{sign}{:02}", strip(mant), exp.abs())
    }
}

// --- where an ask sits in its chain ------------------------------------------------------------------------------------

/// Where the ask being made now sits in its chain (`lineage` in kith-ask).
#[derive(Debug, PartialEq)]
pub struct Place {
    pub chain: Option<String>,
    pub hop: i64,
    pub cross_hops: i64,
    pub ask: Option<String>,
    pub cost_base: f64,
    pub crossed: bool,
}

/// Python's `int(s)` for text from an environment variable: spaces around it, a sign, digits with single `_` between.
fn py_int(s: &str) -> Option<i128> {
    let t = s.trim();
    let digits = t.strip_prefix(['+', '-']).unwrap_or(t);
    if digits.is_empty()
        || digits.starts_with('_')
        || digits.ends_with('_')
        || digits.contains("__")
        || !digits.bytes().all(|b| b.is_ascii_digit() || b == b'_')
    {
        return None;
    }
    // Too big for any number a chain can have: kept as a number that fails the range check, as Python's bigint does.
    let n = digits.replace('_', "").parse::<i128>().unwrap_or(i128::MAX);
    Some(if t.starts_with('-') { -n } else { n })
}

/// Python's `float(s)` for the same: what Rust parses, plus `_` between digits.
fn py_float(s: &str) -> Option<f64> {
    let t = s.trim();
    if !t.contains('_') {
        return t.parse().ok();
    }
    let b = t.as_bytes();
    let between_digits = b.iter().enumerate().all(|(i, c)| {
        *c != b'_'
            || (i > 0 && b[i - 1].is_ascii_digit() && b.get(i + 1).is_some_and(u8::is_ascii_digit))
    });
    if between_digits {
        t.replace('_', "").parse().ok()
    } else {
        None
    }
}

/// The chain this ask joins. `KITH_ASK_ID` is the ask whose run is making it (which only a run `kith ask` started
/// has); a run on another Mac that asked across is handed the chain in `KITH_ASK_*`; anything else starts a chain, at
/// hop 0. The parent's record is looked up in `box_dir`.
pub fn lineage(
    box_dir: &Path,
    env: &dyn Fn(&str) -> Option<String>,
    crossing: bool,
) -> Result<Place, Refusal> {
    lineage_with(&|id| read_reg(box_dir, id), env, crossing)
}

/// [`lineage`] with the parent's record looked up by `parent` instead of read from a file: the seam the core's rows use.
/// `parent` is asked only for a safe id; it answers the upgraded record, or `None` when there is none.
pub fn lineage_with(
    parent: &dyn Fn(&str) -> Option<Reg>,
    env: &dyn Fn(&str) -> Option<String>,
    crossing: bool,
) -> Result<Place, Refusal> {
    let get = |k: &str| env(k).filter(|v| !v.is_empty());
    let mine = get("KITH_ASK_ID").unwrap_or_default();
    let parent = if safe_id(&mine) { parent(&mine) } else { None };
    if let Some(p) = parent {
        return Ok(Place {
            chain: chain_of(&p).map(str::to_owned),
            hop: int_of(&p, "hop") + 1,
            cross_hops: int_of(&p, "cross_hops") + i64::from(crossing),
            ask: Some(mine),
            cost_base: 0.0,
            crossed: crossing,
        });
    }
    let chain = get("KITH_ASK_CHAIN").unwrap_or_default();
    if safe_id(&chain) {
        // the sender's own record is this hop: it is not counted twice
        let hop = get("KITH_ASK_HOP").map_or(Some(0), |v| py_int(&v));
        let cross = get("KITH_ASK_CROSS").map_or(Some(1), |v| py_int(&v));
        let base = get("KITH_ASK_COST").map_or(Some(0.0), |v| py_float(&v));
        let (Some(hop), Some(cross), Some(base)) = (hop, cross, base) else {
            return Err(Refusal::new(
                "delegation: the chain handed to this ask is not numbers",
            ));
        };
        // A chain from another Mac is only as good as its numbers: nothing negative, nothing that skips a cap.
        // (A NaN fails every comparison.)
        if !((0..=64).contains(&hop) && (0..=64).contains(&cross) && (0.0..1e9).contains(&base)) {
            return Err(Refusal::new(
                "delegation: the chain handed to this ask has impossible hop or cost numbers",
            ));
        }
        return Ok(Place {
            chain: Some(chain),
            hop: i64::try_from(hop).unwrap_or(0),
            cross_hops: i64::try_from(cross).unwrap_or(0),
            ask: get("KITH_ASK_FROM"),
            cost_base: base,
            crossed: true,
        });
    }
    Ok(Place {
        chain: None,
        hop: 0,
        cross_hops: i64::from(crossing),
        ask: None,
        cost_base: 0.0,
        crossed: crossing,
    })
}

/// May an ask go out from `place`? `Err` is the refusal `kith ask` exits `REFUSED` with. `spent` is what each chain has
/// cost on this Mac's records (`chain_costs`), `cap` the chain cap.
#[allow(clippy::implicit_hasher)] // always the map `chain_costs` makes
pub fn decide(
    place: &Place,
    spent: &HashMap<String, f64>,
    cap: Option<f64>,
) -> Result<(), Refusal> {
    let cross_max = i64::from(MAX_CROSS_HOPS);
    let local_max = i64::from(MAX_LOCAL_HOPS);
    if place.cross_hops > cross_max {
        return Err(Refusal::new(format!(
            "delegation hop limit: this chain has already crossed to another Mac {cross_max} times (at most {cross_max}); ask the user before starting another chain"
        )));
    }
    if (place.hop - place.cross_hops).max(0) > local_max {
        return Err(Refusal::new(format!(
            "delegation hop limit: this chain is already {local_max} asks deep on this Mac (at most {local_max}); ask the user before starting another chain"
        )));
    }
    let Some(chain) = place.chain.as_deref() else {
        return Ok(());
    };
    // A chain that came back to this Mac already counts here what it carried: the larger, not the sum.
    let spent = place
        .cost_base
        .max(spent.get(chain).copied().unwrap_or(0.0));
    if let Some(cap) = cap.filter(|cap| spent >= *cap) {
        return Err(Refusal::new(format!(
            "delegation cost cap: chain {chain} has spent US${spent:.2} of its US${} cap; ask the user before starting another ask",
            pct_g(cap)
        )));
    }
    Ok(())
}

/// What `open_record` decides for ask `id` before it writes the record: the chain, hop and cross hops the record would
/// get, or the refusal. A reading only: the lock is gone when this returns, so a caller that goes on to WRITE the record
/// must use [`check_and_write`], or another ask could be admitted between the two.
pub fn check(
    machine: &Path,
    env: &dyn Fn(&str) -> Option<String>,
    id: &str,
    crossing: bool,
) -> Result<Value, Refusal> {
    check_and_write(machine, env, id, crossing, |_| Ok(()))
}

/// [`check`] and the write of the record as one step under the box lock (kith-ask's `box_locked`: check, then append):
/// `write` gets the decision and runs only when the ask is admitted; the lock is held until it returns. A box lock that
/// cannot be taken refuses the ask (the Python raises): an answer the writer could not have seen is no answer.
pub fn check_and_write(
    machine: &Path,
    env: &dyn Fn(&str) -> Option<String>,
    id: &str,
    crossing: bool,
    write: impl FnOnce(&Value) -> std::io::Result<()>,
) -> Result<Value, Refusal> {
    let dir = box_dir(machine);
    // Reading the records, as one step with whoever appends them (kith-ask's box_locked). Held until the end.
    let _lock = std::fs::create_dir_all(&dir)
        .and_then(|()| File::create(dir.join(".lock")))
        .and_then(|f| f.lock_exclusive().map(|()| f))
        .map_err(|e| Refusal::new(format!("can't lock the ask box ({}): {e}", dir.display())))?;
    let place = lineage(&dir, env, crossing)?;
    let costs = if place.chain.is_some() {
        chain_costs(&records(&dir))
    } else {
        HashMap::new()
    };
    decide(&place, &costs, chain_cost_cap(machine))?;
    let decision = decision_json(&place, id);
    write(&decision).map_err(|e| Refusal::new(format!("can't write the ask's record: {e}")))?;
    Ok(decision)
}

/// What [`check`] answers for an admitted ask `id` at `place`: the chain (the ask's own id when it starts one), hop, cross
/// hops and the rest the record gets.
pub fn decision_json(place: &Place, id: &str) -> Value {
    json!({
        "chain": place.chain.as_deref().unwrap_or(id),
        "hop": place.hop,
        "cross_hops": place.cross_hops,
        "crossed": place.crossed,
        "ask": place.ask,
        "cost_base": place.cost_base,
    })
}

/// What `kith kernel delegation limits` prints.
pub fn limits_json() -> Value {
    json!({
        "max_local_hops": MAX_LOCAL_HOPS,
        "max_cross_hops": MAX_CROSS_HOPS,
        "refused_exit": REFUSED,
        "default_cap_usd": DEFAULT_CAP_USD,
    })
}

#[cfg(test)]
mod tests;
