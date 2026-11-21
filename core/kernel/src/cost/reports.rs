//! The reports of `kith-cost` a person reads: `today|week|month|all`, `agents`, `agent NAME`, `people`, `session PREFIX` and
//! `csv`. Their periods start from the real clock (`since_of`; `KITH_COST_NOW` moves only the apps' views), and the numbers
//! are printed as Python's f-strings print them (`pyfmt`).

use super::clock::Now;
use super::{Out, Paths, pyfmt};
use rusqlite::{Connection, params, types::ValueRef};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

type R<T> = rusqlite::Result<T>;

/// `money(x)`: dollars with a comma every three digits, or `n/a` for a request with no price.
fn money(x: Option<f64>) -> String {
    x.map_or_else(
        || "   n/a".to_owned(),
        |x| format!("${}", pyfmt::commas(x, 2)),
    )
}

/// `s[:n]` of a string, by characters.
fn head(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// `s[a:b]` of a string, by characters.
fn slice(s: &str, a: usize, b: usize) -> String {
    s.chars().skip(a).take(b.saturating_sub(a)).collect()
}

/// `repr(s)` of a string.
fn repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::from(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || (0x7f..0xa0).contains(&(c as u32)) => {
                let _ = write!(out, "\\x{:02x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// What `kith-cost` dies with: the message on stderr, exit 1 (`sys.exit(msg)`).
fn die(msg: &str) -> Out {
    Out::fail(format!("{msg}\n"), 1)
}

fn broken(e: &dyn std::fmt::Display) -> Out {
    Out::fail(format!("kith-cost: {e}\n"), 1)
}

/// `since_of(period)`: where a period starts, as the ledger writes its stamps. Today is since local midnight, a week 7 days
/// back and a month 30 days back (not the calendar month); any other word is everything (`0`). The real clock, always.
fn since_of(period: &str) -> String {
    since_at(&Now::at(chrono::Local::now().fixed_offset()), period)
}

/// `since_of` at a given instant.
pub(super) fn since_at(now: &Now, period: &str) -> String {
    match period {
        "today" => Now::utc_text(now.midnight()),
        "week" => Now::utc_text(now.0 - chrono::Duration::days(7)),
        "month" => Now::utc_text(now.0 - chrono::Duration::days(30)),
        _ => "0".to_owned(),
    }
}

fn text(r: &rusqlite::Row<'_>, i: usize) -> R<String> {
    Ok(r.get::<_, Option<String>>(i)?.unwrap_or_default())
}

/// `report(c, period)`: totals, by day, by project, by model and the top sessions.
pub(super) fn report(c: &Connection, p: &Paths, period: &str) -> Out {
    one(report_text(c, p, period))
}

fn one(r: R<String>) -> Out {
    match r {
        Ok(s) => Out::ok(s),
        Err(e) => broken(&e),
    }
}

fn report_text(c: &Connection, p: &Paths, period: &str) -> R<String> {
    let since = since_of(period);
    let (cost, n, output, cache_read): (Option<f64>, i64, Option<f64>, Option<f64>) = c.query_row(
        "select sum(cost), count(*), sum(output), sum(cache_read) from req where ts >= ?",
        [&since],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    let mut o = String::new();
    let _ = write!(
        o,
        "\n{period}: {} over {n} requests ({}M output tokens, {}M cache reads)\n\n",
        money(Some(cost.unwrap_or(0.0))),
        pyfmt::fixed(output.unwrap_or(0.0) / 1e6, 2),
        pyfmt::fixed(cache_read.unwrap_or(0.0) / 1e6, 0),
    );
    o.push_str("by day\n");
    let mut st = c.prepare(
        "select substr(datetime(ts,'localtime'),1,10) d, sum(cost), count(*) from req where ts >= ? group by d order by d desc limit 14",
    )?;
    for row in st.query_map([&since], |r| {
        Ok((
            text(r, 0)?,
            r.get::<_, Option<f64>>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })? {
        let (day, cost, n) = row?;
        let _ = writeln!(o, "  {day}  {:>9}  {n:5} req", money(cost));
    }
    o.push_str("\nby project\n");
    let home = p.home.to_string_lossy().into_owned();
    let mut st = c.prepare(
        "select project, sum(cost) s, count(*) from req where ts >= ? group by project order by s desc limit 10",
    )?;
    for row in st.query_map([&since], |r| {
        Ok((
            text(r, 0)?,
            r.get::<_, Option<f64>>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })? {
        let (project, cost, n) = row?;
        let shown = if home.is_empty() {
            project
        } else {
            project.replace(&home, "~")
        };
        let shown = if shown.is_empty() {
            "?".to_owned()
        } else {
            shown
        };
        let _ = writeln!(o, "  {:>9}  {n:5} req  {shown}", money(cost));
    }
    o.push_str("\nby model\n");
    let mut st = c.prepare(
        "select model, sum(cost) s, count(*) from req where ts >= ? group by model order by s desc",
    )?;
    for row in st.query_map([&since], |r| {
        Ok((
            r.get::<_, Option<String>>(0)?,
            r.get::<_, Option<f64>>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })? {
        let (model, cost, n) = row?;
        let _ = writeln!(
            o,
            "  {:>9}  {n:5} req  {}",
            money(cost),
            model.unwrap_or_else(|| "None".to_owned())
        );
    }
    o.push_str("\ntop sessions  (kith-cost session <id> for turns)\n");
    let mut st = c.prepare(
        "select session, sum(cost) s, min(datetime(ts,'localtime')),
            (select turn from req r2 where r2.session = req.session and turn is not null order by ts limit 1)
            from req where ts >= ? group by session order by s desc limit 8",
    )?;
    for row in st.query_map([&since], |r| {
        Ok((
            text(r, 0)?,
            r.get::<_, Option<f64>>(1)?,
            text(r, 2)?,
            text(r, 3)?,
        ))
    })? {
        let (session, cost, first, turn) = row?;
        let _ = writeln!(
            o,
            "  {:>9}  {}  {}  {}",
            money(cost),
            head(&session, 8),
            head(&first, 16),
            head(&turn, 60)
        );
    }
    o.push('\n');
    Ok(o)
}

/// `agents [period]`: the kith agents side by side.
pub(super) fn agents(c: &Connection, period: &str) -> Out {
    one(agents_text(c, period))
}

fn agents_text(c: &Connection, period: &str) -> R<String> {
    let since = since_of(period);
    let mut st = c.prepare(
        "select hq_agent, sum(cost) s, count(distinct session), count(*), sum(output), group_concat(distinct model)
         from hq_req where hq_agent is not null and ts >= ? group by hq_agent order by s desc",
    )?;
    let rows: Vec<_> = st
        .query_map([&since], |r| {
            Ok((
                text(r, 0)?,
                r.get::<_, Option<f64>>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, Option<f64>>(4)?,
                text(r, 5)?,
            ))
        })?
        .collect::<R<_>>()?;
    let total: Option<f64> =
        c.query_row("select sum(cost) from req where ts >= ?", [&since], |r| {
            r.get(0)
        })?;
    let kith = rows.iter().fold(0.0, |t, r| t + r.1.unwrap_or(0.0));
    let mut o = format!(
        "\nkith agents, {period}: {} of {} total Claude Code usage\n\n",
        money(Some(kith)),
        money(Some(total.unwrap_or(0.0)))
    );
    let _ = writeln!(
        o,
        "  {:<12}{:>10}{:>6}{:>10}{:>7}{:>9}  models",
        "agent", "cost", "runs", "per run", "req", "out tok"
    );
    for (agent, cost, runs, n, out, models) in &rows {
        let short: BTreeSet<String> = models
            .split(',')
            .filter(|m| !m.is_empty())
            .map(|m| m.replace("claude-", ""))
            .collect();
        let short = short.into_iter().collect::<Vec<_>>().join(", ");
        let per_run = cost.unwrap_or(0.0) / *runs as f64;
        let out = pyfmt::fixed(out.unwrap_or(0.0) / 1e3, 0);
        let _ = writeln!(
            o,
            "  {agent:<12}{:>10}{runs:>6}{:>10}{n:>7}{:>8}k  {short}",
            money(*cost),
            money(Some(per_run)),
            out
        );
    }
    if rows.is_empty() {
        o.push_str("  no kith runs recorded yet (runs are tagged from now on; kith:<name> subagents count from the start)\n");
    }
    o.push_str("\n  runs = sessions started by `kith <agent>` plus sessions that called it as a kith:<name> subagent\n\n");
    Ok(o)
}

/// `agent NAME [period]`: one kith agent by day, version, model and effort, and its costliest runs.
pub(super) fn agent(c: &Connection, name: &str, period: &str) -> Out {
    match agent_text(c, name, period) {
        Ok(Ok(s)) => Out::ok(s),
        Ok(Err(msg)) => die(&msg),
        Err(e) => broken(&e),
    }
}

fn agent_text(c: &Connection, name: &str, period: &str) -> R<Result<String, String>> {
    let since = since_of(period);
    let (cost, runs, n, out): (Option<f64>, i64, i64, Option<f64>) = c.query_row(
        "select sum(cost), count(distinct session), count(*), sum(output) from hq_req where hq_agent = ? and ts >= ?",
        params![name, since],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )?;
    if n == 0 {
        return Ok(Err(format!(
            "no usage recorded for kith agent {} ({period})",
            repr(name)
        )));
    }
    let mut o = format!(
        "\n{name}, {period}: {} over {runs} runs, {n} requests, {}k output tokens\n\n",
        money(cost),
        pyfmt::fixed(out.unwrap_or(0.0) / 1e3, 0)
    );
    o.push_str("by day\n");
    let mut st = c.prepare(
        "select substr(datetime(ts,'localtime'),1,10) d, sum(cost), count(distinct session) from hq_req
         where hq_agent = ? and ts >= ? group by d order by d desc limit 14",
    )?;
    for row in st.query_map(params![name, since], |r| {
        Ok((
            text(r, 0)?,
            r.get::<_, Option<f64>>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })? {
        let (day, cost, runs) = row?;
        let _ = writeln!(o, "  {day}  {:>9}  {runs:3} runs", money(cost));
    }
    o.push_str("\nby version, model and effort  (version and effort are known for `kith <agent>` runs only)\n");
    let mut st = c.prepare(
        "select coalesce(version, '-'), model, coalesce(effort, '-'), via, sum(cost) s, count(distinct session)
         from hq_req where hq_agent = ? and ts >= ? group by 1, 2, 3, 4 order by s desc limit 15",
    )?;
    for row in st.query_map(params![name, since], |r| {
        Ok((
            text(r, 0)?,
            r.get::<_, Option<String>>(1)?,
            text(r, 2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<f64>>(4)?,
            r.get::<_, i64>(5)?,
        ))
    })? {
        let (ver, model, eff, via, cost, runs) = row?;
        let _ = writeln!(
            o,
            "  {:>9}  {runs:3} runs  {ver:<11} {:<22} {eff:<7} {}",
            money(cost),
            model.unwrap_or_else(|| "None".to_owned()),
            via.unwrap_or_else(|| "None".to_owned())
        );
    }
    o.push_str("\ncostliest runs  (kith-cost session <id> for turns)\n");
    let mut st = c.prepare(
        "select session, sum(cost) s, min(datetime(ts,'localtime')),
            (select turn from req r2 where r2.session = hq_req.session and turn is not null order by ts limit 1)
            from hq_req where hq_agent = ? and ts >= ? group by session order by s desc limit 10",
    )?;
    for row in st.query_map(params![name, since], |r| {
        Ok((
            text(r, 0)?,
            r.get::<_, Option<f64>>(1)?,
            text(r, 2)?,
            text(r, 3)?,
        ))
    })? {
        let (session, cost, first, turn) = row?;
        let _ = writeln!(
            o,
            "  {:>9}  {}  {}  {}",
            money(cost),
            head(&session, 8),
            head(&first, 16),
            head(&turn, 60)
        );
    }
    o.push('\n');
    Ok(Ok(o))
}

/// `{bot id: (root person, label)}` from `kith-auth people --json`; empty when kith-auth has no bots or can't answer.
fn bot_tree(p: &Paths) -> BTreeMap<String, (String, String)> {
    let mut cmd = std::process::Command::new(p.kernel.join("bin").join("kith-auth"));
    cmd.args(["people", "--json"]);
    let Some(out) =
        crate::secrets::run_command_with_timeout(cmd, std::time::Duration::from_secs(20))
    else {
        return BTreeMap::new();
    };
    let Ok(Value::Array(people)) = serde_json::from_str::<Value>(&out) else {
        return BTreeMap::new();
    };
    let str_of = |x: &Value, k: &str| x.get(k).and_then(Value::as_str).map(str::to_owned);
    let truthy = |x: &Value, k: &str| super::ledger::truthy(x.get(k));
    let owner = people
        .iter()
        .find(|x| truthy(x, "owner"))
        .and_then(|x| str_of(x, "id"));
    people
        .iter()
        .filter(|x| truthy(x, "bot_of"))
        .filter_map(|x| {
            let id = str_of(x, "id")?;
            let root = str_of(x, "root")?;
            let root = if Some(&root) == owner.as_ref() {
                "owner".to_owned()
            } else {
                root
            };
            let name = str_of(x, "name")
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| id.clone());
            let provider = str_of(x, "provider")
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| "other".to_owned());
            Some((id, (root, format!("{name} ({provider})"))))
        })
        .collect()
}

/// `people [period]`: what each person's runs cost, by agent; a bot's runs count on its owner's line.
pub(super) fn people(c: &Connection, p: &Paths, period: &str) -> Out {
    one(people_text(c, p, period))
}

/// One person's line: their own runs by agent, and their bots (label, cost) in the order they first showed up.
#[derive(Default)]
struct Group {
    own: Vec<(String, Option<f64>, i64)>,
    bots: Vec<(String, f64)>,
}

fn people_text(c: &Connection, p: &Paths, period: &str) -> R<String> {
    let since = since_of(period);
    let mut st = c.prepare(
        "select coalesce(person, 'owner'), hq_agent, sum(cost) s, count(distinct session)
         from hq_req where hq_agent is not null and ts >= ? group by 1, 2 order by 1, s desc",
    )?;
    let rows: Vec<_> = st
        .query_map([&since], |r| {
            Ok((
                text(r, 0)?,
                text(r, 1)?,
                r.get::<_, Option<f64>>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?
        .collect::<R<_>>()?;
    let bots = bot_tree(p);
    // a person's line holds their own runs and their bots'; each bot is a sub-line under it
    let mut groups: BTreeMap<String, Group> = BTreeMap::new();
    let mut bot_slot: BTreeMap<String, usize> = BTreeMap::new();
    for (who, agent, cost, runs) in &rows {
        if let Some((root, label)) = bots.get(who) {
            let g = groups.entry(root.clone()).or_default();
            let i = *bot_slot.entry(who.clone()).or_insert_with(|| {
                g.bots.push((label.clone(), 0.0));
                g.bots.len() - 1
            });
            g.bots[i].1 += cost.unwrap_or(0.0);
        } else {
            groups
                .entry(who.clone())
                .or_default()
                .own
                .push((agent.clone(), *cost, *runs));
        }
    }
    let mut o = format!("\nwhat each person's runs cost, {period}  (owner: your own runs)\n\n");
    for (who, g) in &mut groups {
        let total = g.own.iter().fold(0.0, |t, x| t + x.1.unwrap_or(0.0))
            + g.bots.iter().fold(0.0, |t, b| t + b.1);
        let _ = writeln!(o, "  {who:<12}{:>10}", money(Some(total)));
        for (agent, cost, runs) in &g.own {
            let _ = writeln!(o, "    {agent:<14}{:>10}{runs:>5} runs", money(*cost));
        }
        // stable, like Python's sorted(key=-cost): equal costs keep the order they came in
        g.bots
            .sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        for (label, cost) in &g.bots {
            let _ = writeln!(o, "    └ {label:<30}{:>10}", money(Some(*cost)));
        }
    }
    if rows.is_empty() {
        o.push_str("  nothing recorded yet\n");
    }
    o.push('\n');
    Ok(o)
}

/// `session PREFIX`: one session turn by turn, plus its subagents.
pub(super) fn session(c: &Connection, prefix: &str) -> Out {
    match session_text(c, prefix) {
        Ok(Ok(s)) => Out::ok(s),
        Ok(Err(msg)) => die(&msg),
        Err(e) => broken(&e),
    }
}

fn session_text(c: &Connection, prefix: &str) -> R<Result<String, String>> {
    let mut st = c.prepare("select distinct session from req where session like ?")?;
    let ids: Vec<String> = st
        .query_map([format!("{prefix}%")], |r| text(r, 0))?
        .collect::<R<_>>()?;
    let [sid] = ids.as_slice() else {
        return Ok(Err(format!(
            "{} sessions match {}",
            ids.len(),
            repr(prefix)
        )));
    };
    let mut o = format!(
        "\nsession {sid}\n\nturns (main conversation; cost of everything until the next prompt)\n"
    );
    let mut st = c.prepare(
        "select turn, sum(cost), count(*), sum(output), sum(cache_read), min(datetime(ts,'localtime'))
         from req where session = ? and agent = 'main' group by turn order by min(ts)",
    )?;
    for row in st.query_map([sid], |r| {
        Ok((
            r.get::<_, Option<String>>(0)?,
            r.get::<_, Option<f64>>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, Option<f64>>(3)?,
            r.get::<_, Option<f64>>(4)?,
            text(r, 5)?,
        ))
    })? {
        let (turn, cost, n, out, cr, t) = row?;
        let turn = turn
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| "(start)".to_owned());
        let _ = writeln!(
            o,
            "  {:>8}  {n:4} req  {:>6}k out  {:>5}M cached  {}  {}",
            money(cost),
            pyfmt::fixed(out.unwrap_or(0.0) / 1e3, 1),
            pyfmt::fixed(cr.unwrap_or(0.0) / 1e6, 1),
            slice(&t, 5, 16),
            head(&turn, 60)
        );
    }
    let mut st = c.prepare(
        "select agent, sum(cost) s, count(*) from req where session = ? and agent != 'main' group by agent order by s desc",
    )?;
    let subs: Vec<_> = st
        .query_map([sid], |r| {
            Ok((
                text(r, 0)?,
                r.get::<_, Option<f64>>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?
        .collect::<R<_>>()?;
    if !subs.is_empty() {
        o.push_str("\nsubagents\n");
        for (agent, cost, n) in subs {
            let _ = writeln!(o, "  {:>8}  {n:4} req  {}", money(cost), head(&agent, 80));
        }
    }
    let total: Option<f64> =
        c.query_row("select sum(cost) from req where session = ?", [sid], |r| {
            r.get(0)
        })?;
    let _ = write!(o, "\ntotal {}\n\n", money(total));
    Ok(Ok(o))
}

/// One CSV field, as Python's `csv.writer` (excel dialect) writes it: quoted only when it holds a comma, a quote or a line break.
fn field(out: &mut String, s: &str) {
    if s.contains([',', '"', '\r', '\n']) {
        out.push('"');
        out.push_str(&s.replace('"', "\"\""));
        out.push('"');
    } else {
        out.push_str(s);
    }
}

/// `csv [since YYYY-MM-DD]`: every request as CSV with its kith agent; the date is the third word.
pub(super) fn csv(c: &Connection, since: &str) -> Out {
    one(csv_text(c, since))
}

fn csv_text(c: &Connection, since: &str) -> R<String> {
    let mut o = String::new();
    let header = [
        "id",
        "session",
        "ts",
        "project",
        "model",
        "agent",
        "turn",
        "input",
        "output",
        "cache_read",
        "cache_write",
        "cost",
        "hq_agent",
        "version",
        "via",
        "effort",
        "person",
    ];
    for (i, h) in header.iter().enumerate() {
        if i > 0 {
            o.push(',');
        }
        field(&mut o, h);
    }
    o.push_str("\r\n");
    let mut st = c.prepare("select * from hq_req where ts >= ? order by ts")?;
    let n = st.column_count();
    let mut rows = st.query([since])?;
    while let Some(r) = rows.next()? {
        for i in 0..n {
            if i > 0 {
                o.push(',');
            }
            match r.get_ref(i)? {
                ValueRef::Null => {}
                ValueRef::Integer(x) => o.push_str(&x.to_string()),
                ValueRef::Real(x) => field(&mut o, &real(x)),
                ValueRef::Text(t) => field(&mut o, &String::from_utf8_lossy(t)),
                ValueRef::Blob(b) => field(&mut o, &format!("b'{}'", String::from_utf8_lossy(b))),
            }
        }
        o.push_str("\r\n");
    }
    Ok(o)
}

/// `repr(x)` of a float.
fn real(x: f64) -> String {
    serde_json::Number::from_f64(x).map_or_else(
        || {
            if x.is_nan() {
                "nan".to_owned()
            } else if x > 0.0 {
                "inf".to_owned()
            } else {
                "-inf".to_owned()
            }
        },
        |n| pyfmt::number_str(&n),
    )
}
