//! What the apps and the launch read out of the ledger: `limit`, `spent`, `usage --json`, `agent NAME --json`,
//! `members` and `bots --json` of `kernel/bin/kith-cost`, with the same numbers, words and rounding.

use super::clock::{self, Now};
use super::{Paths, pyfmt};
use chrono::{Datelike as _, Duration, NaiveDate};
use rusqlite::{Connection, OptionalExtension as _, params, params_from_iter};
use serde_json::{Map, Number, Value, json};
use std::fmt::Write as _;
use std::process::Command;
use std::time::Duration as Wait;

type R<T> = rusqlite::Result<T>;

/// A Python number: an `int` or a `float` (the JSON the apps read tells them apart only by a trailing `.0`).
#[derive(Clone, Copy)]
enum Py {
    I(i64),
    F(f64),
}

impl Py {
    fn f(self) -> f64 {
        match self {
            Py::I(i) => i as f64,
            Py::F(f) => f,
        }
    }

    fn value(self) -> Value {
        match self {
            Py::I(i) => Value::from(i),
            Py::F(f) => Value::from(f),
        }
    }

    /// `x or 0` for a sum that may be NULL (an `int` zero when it is nothing or `0.0`, else the float).
    fn or_zero(x: Option<f64>) -> Py {
        x.filter(|v| *v != 0.0).map_or(Py::I(0), Py::F)
    }

    /// `round(self, n)`: an `int` stays one.
    fn round(self, n: usize) -> Py {
        match self {
            Py::I(i) => Py::I(i),
            Py::F(f) => Py::F(pyfmt::round(f, n)),
        }
    }

    fn sub(self, o: Py) -> Py {
        match (self, o) {
            (Py::I(a), Py::I(b)) => Py::I(a - b),
            (a, b) => Py::F(a.f() - b.f()),
        }
    }

    fn add(self, o: Py) -> Py {
        match (self, o) {
            (Py::I(a), Py::I(b)) => Py::I(a + b),
            (a, b) => Py::F(a.f() + b.f()),
        }
    }

    /// `max(self, 0)`: `self` unless it is below zero (a tie keeps the first, so `0.0` stays a float).
    fn at_least_zero(self) -> Py {
        if self.f() < 0.0 { Py::I(0) } else { self }
    }
}

fn rnd(x: f64, n: usize) -> Value {
    Py::F(x).round(n).value()
}

/// What a monthly limit says in this Mac's settings: the amount (`None` when there is none) and what happens at it.
pub struct Limit {
    pub amount: Option<Number>,
    pub at: String,
}

impl Limit {
    fn value(&self) -> f64 {
        self.amount.as_ref().and_then(Number::as_f64).unwrap_or(0.0)
    }
}

/// `kith_settings.limit(agent)`: a settings file that cannot be used sets no limit.
pub fn limit_of(p: &Paths, agent: &str) -> Limit {
    let s = crate::accounts::read_settings(&p.machine);
    let a = s
        .get("agents")
        .and_then(Value::as_object)
        .and_then(|a| a.get(agent))
        .and_then(Value::as_object);
    let amount = a
        .and_then(|a| a.get("limit"))
        .and_then(Value::as_number)
        .filter(|n| n.as_f64().is_some_and(|f| f != 0.0))
        .cloned();
    let at = a
        .and_then(|a| a.get("at_limit"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("ask")
        .to_owned();
    Limit { amount, at }
}

fn dollars(x: f64) -> String {
    format!(
        "${}",
        if x >= 10.0 {
            pyfmt::commas(x, 0)
        } else {
            pyfmt::commas(x, 2)
        }
    )
}

fn short_day(d: NaiveDate) -> String {
    format!("{} {}", d.format("%b"), d.day())
}

pub fn period_label(now: &Now, period: &str) -> String {
    match period {
        "today" => "Today".to_owned(),
        "week" => "Last 7 days".to_owned(),
        "month" => now.month_name(),
        _ => "All time".to_owned(),
    }
}

pub fn month_spent(c: &Connection, now: &Now, agent: &str) -> R<f64> {
    let since = Now::utc_text(now.month_start());
    let s: Option<f64> = c.query_row(
        "select sum(cost) from hq_req where hq_agent = ? and ts >= ?",
        params![agent, since],
        |r| r.get(0),
    )?;
    Ok(s.unwrap_or(0.0))
}

/// Every day of this month up to today (local time) and what the agent cost on it.
pub fn month_days(c: &Connection, now: &Now, agent: &str) -> R<Vec<(NaiveDate, f64)>> {
    let mut st = c.prepare(
        "select substr(datetime(ts,'localtime'),1,10) d, sum(cost) from hq_req where hq_agent = ? and ts >= ? group by d",
    )?;
    let rows: Vec<(String, Option<f64>)> = st
        .query_map(params![agent, Now::utc_text(now.month_start())], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?
        .collect::<R<_>>()?;
    let today = now.date();
    let first = today.with_day(1).unwrap_or(today);
    Ok((0..today.day())
        .map(|i| {
            let d = first + Duration::days(i64::from(i));
            let iso = d.format("%Y-%m-%d").to_string();
            let cost = rows
                .iter()
                .find(|(k, _)| *k == iso)
                .and_then(|(_, x)| *x)
                .unwrap_or(0.0);
            (d, cost)
        })
        .collect())
}

/// The pace and what it means, in words, plus the day each round amount is reached.
pub fn forecast(now: &Now, days: &[(NaiveDate, f64)], spent: f64, limit: Option<&Number>) -> Value {
    let lim = limit.and_then(Number::as_f64).filter(|l| *l != 0.0);
    let today = now.date();
    let end = clock::month_end(today);
    let recent: Vec<(NaiveDate, f64)> = days
        .iter()
        .copied()
        .filter(|(d, _)| (today - *d).num_days() < 7)
        .collect();
    let since = recent.iter().find(|(_, x)| *x > 0.0).map(|(d, _)| *d);
    let per_day = since.map_or(0.0, |s| {
        recent
            .iter()
            .filter(|(d, _)| *d >= s)
            .fold(0.0, |a, (_, x)| a + x)
            / (((today - s).num_days() + 1) as f64)
    });
    let left_today = now.left_today();

    // The day `amount` is (or was) reached this month, or None when it lasts the month.
    let reach = |amount: f64| -> Option<NaiveDate> {
        let mut total = 0.0;
        for (d, x) in days {
            total += x;
            if total >= amount {
                return Some(*d);
            }
        }
        if per_day <= 0.0 {
            return None;
        }
        let by_tonight = spent + per_day * left_today;
        if by_tonight >= amount {
            return Some(today);
        }
        let n = -pyfmt::floordiv(-(amount - by_tonight), per_day);
        // Past the month's end it is None whatever the date: stay clear of a date out of range.
        if n > 400.0 {
            return None;
        }
        let d = today + Duration::days(n as i64);
        (d <= end).then_some(d)
    };

    let month = now.month_name();
    let text = match since {
        None => {
            let mut t = if spent == 0.0 {
                format!("Nothing spent in {month} yet.")
            } else {
                "Nothing spent in the last 7 days.".to_owned()
            };
            if let Some(l) = lim {
                let _ = write!(t, " {} is the limit for {month}.", dollars(l));
            }
            t
        }
        Some(since) => {
            let pace = format!(
                "About {} a day since {}",
                dollars(per_day),
                short_day(since)
            );
            let hit = lim.and_then(reach);
            match lim {
                Some(l) if spent >= l => match hit {
                    Some(h) => format!("{pace}; it reached {} on {}.", dollars(l), short_day(h)),
                    None => format!("{pace}; it is at {}.", dollars(l)),
                },
                Some(l) if hit == Some(today) => {
                    format!("{pace}, so it reaches {} today.", dollars(l))
                }
                Some(l) if hit == Some(today + Duration::days(1)) => {
                    format!("{pace}, so it reaches {} tomorrow.", dollars(l))
                }
                Some(l) if hit.is_some() => format!(
                    "{pace}, so it reaches {} about {}.",
                    dollars(l),
                    hit.map(short_day).unwrap_or_default()
                ),
                Some(l) => format!("{pace}, so it stays under {} in {month}.", dollars(l)),
                None => {
                    let rest = (end - today).num_days() as f64 + left_today;
                    format!(
                        "{pace}, about {} by the end of {month}.",
                        dollars(spent + per_day * rest)
                    )
                }
            }
        }
    };

    // The amounts the limit sheet offers; a limit equal to one of them is that one (the set keeps the first).
    let mut amounts: Vec<(f64, String)> = [50, 100, 200, 300, 400, 500, 750, 1000]
        .iter()
        .map(|a| (f64::from(*a), a.to_string()))
        .collect();
    if let (Some(l), Some(n)) = (lim, limit)
        && !amounts.iter().any(|(a, _)| *a == l)
    {
        amounts.push((l, pyfmt::number_str(n)));
    }
    amounts.sort_by(|a, b| a.0.total_cmp(&b.0));
    let iso = |d: NaiveDate| Value::String(d.format("%Y-%m-%d").to_string());
    let reach_map: Map<String, Value> = amounts
        .iter()
        .map(|(a, key)| (key.clone(), reach(*a).map_or(Value::Null, iso)))
        .collect();
    json!({
        "perDay": pyfmt::round(per_day, 2),
        "since": since.map_or(Value::Null, iso),
        "text": text,
        "monthEnd": iso(end),
        "reach": reach_map,
    })
}

/// The next limit a card offers: $100 more from $100 up, double below; always above what is spent.
fn raise_to(limit: f64, spent: f64) -> Value {
    let step = if limit >= 100.0 {
        100.0
    } else {
        10.0_f64.max(limit)
    };
    let mut to = limit + step;
    while to <= spent {
        to += step;
    }
    if to == to.trunc() {
        Value::from(to as i64)
    } else {
        Value::from(pyfmt::round(to, 2))
    }
}

fn limit_state(c: &Connection, now: &Now, agent: &str, limit: &Limit) -> R<Map<String, Value>> {
    let amount = limit.value();
    let spent = month_spent(c, now, agent)?;
    let runs: i64 = c.query_row(
        "select count(distinct session) from hq_req where hq_agent = ? and ts >= ?",
        params![agent, Now::utc_text(now.month_start())],
        |r| r.get(0),
    )?;
    let month = now.month_name();
    let mut st = Map::new();
    st.insert("agent".into(), json!(agent));
    st.insert(
        "limit".into(),
        limit.amount.clone().map_or(Value::Null, Value::Number),
    );
    st.insert("at".into(), json!(limit.at));
    st.insert("spent".into(), rnd(spent, 4));
    st.insert("over".into(), json!(spent >= amount));
    st.insert("month".into(), json!(month));
    st.insert("runs".into(), json!(runs));
    st.insert(
        "perRun".into(),
        if runs != 0 {
            rnd(spent / runs as f64, 2)
        } else {
            json!(0)
        },
    );
    st.insert("raiseTo".into(), raise_to(amount, spent));
    st.insert(
        "resets".into(),
        json!(now.next_month().format("%Y-%m-%d").to_string()),
    );
    st.insert(
        "text".into(),
        json!(format!(
            "{agent} has used ${} of its {} for {month} (at API prices)",
            pyfmt::commas(spent, 2),
            dollars(amount)
        )),
    );
    Ok(st)
}

/// `kith-cost limit AGENT [--json|--sh]`. The ledger is opened by the caller only when there is a limit.
pub fn limit_cmd(c: &Connection, now: &Now, agent: &str, limit: &Limit, fmt: &str) -> R<String> {
    let mut st = limit_state(c, now, agent, limit)?;
    let spent = st.get("spent").and_then(Value::as_f64).unwrap_or(0.0);
    let days = month_days(c, now, agent)?;
    let fc = forecast(now, &days, spent, limit.amount.as_ref());
    let get = |k: &str| st.get(k).cloned().unwrap_or(Value::Null);
    let text = get("text").as_str().unwrap_or_default().to_owned();
    let out = match fmt {
        "--sh" => format!(
            "l_over={} l_at={} l_limit={} l_spent={} l_text={}\n",
            i32::from(get("over") == Value::Bool(true)),
            pyfmt::shell_quote(&limit.at),
            pyfmt::shell_quote(
                &limit
                    .amount
                    .as_ref()
                    .map(pyfmt::number_str)
                    .unwrap_or_default()
            ),
            pyfmt::fixed(spent, 2),
            pyfmt::shell_quote(&text),
        ),
        "--json" => {
            st.insert("forecast".into(), fc);
            format!("{}\n", crate::secrets::pyjson::dumps(&Value::Object(st)))
        }
        _ => format!(
            "{text}; at the limit: {}.\n{}\n",
            limit.at,
            fc["text"].as_str().unwrap_or_default()
        ),
    };
    Ok(out)
}

/// `kith-cost spent PERSON [AGENT]`: this calendar month's cost for a person (their old ids count too), four decimals.
pub fn spent(c: &Connection, now: &Now, person: &str, agent: Option<&str>, was: &str) -> R<String> {
    let mut ids = vec![person.to_owned()];
    ids.extend(was.split(',').filter(|x| !x.is_empty()).map(str::to_owned));
    let marks = vec!["?"; ids.len()].join(",");
    let mut sql = format!("select sum(cost) from hq_req where ts >= ? and person in ({marks})");
    let mut args = vec![Now::utc_text(now.month_start())];
    args.extend(ids);
    if let Some(a) = agent.filter(|a| !a.is_empty()) {
        sql.push_str(" and hq_agent = ?");
        args.push(a.to_owned());
    }
    let s: Option<f64> = c.query_row(&sql, params_from_iter(args), |r| r.get(0))?;
    Ok(format!("{}\n", pyfmt::fixed(s.unwrap_or(0.0), 4)))
}

/// `usage --json [PERIOD]`: the total, agents against your own sessions, one row per agent with its month and its limit.
pub fn usage_json(c: &Connection, p: &Paths, now: &Now, period: &str) -> R<String> {
    let since = now.app_since(period);
    let mut st = c.prepare(
        "select hq_agent, sum(cost) s, count(distinct session) from hq_req where hq_agent is not null and ts >= ? group by hq_agent order by s desc",
    )?;
    let rows: Vec<(String, Option<f64>, i64)> = st
        .query_map(params![since], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<R<_>>()?;
    let total: Option<f64> = c.query_row(
        "select sum(cost) from req where ts >= ?",
        params![since],
        |r| r.get(0),
    )?;
    let total = Py::or_zero(total);

    let settings = crate::accounts::read_settings(&p.machine);
    let limits: Vec<(String, &Map<String, Value>)> = settings
        .get("agents")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(a, v)| {
            v.as_object()
                .filter(|o| super::ledger::truthy(o.get("limit")))
                .map(|o| (a.clone(), o))
        })
        .collect();
    let lim = |agent: &str| limits.iter().find(|(a, _)| a == agent).map(|(_, o)| *o);

    let mut out: Vec<(String, Py, i64, Py)> = Vec::new();
    for (a, cost, runs) in &rows {
        // the per-run figure divides the sum as it is, not as rounded for the row
        let per_run = if *runs != 0 {
            Py::F(pyfmt::round(Py::or_zero(*cost).f() / *runs as f64, 2))
        } else {
            Py::I(0)
        };
        out.push((a.clone(), Py::or_zero(*cost).round(4), *runs, per_run));
    }
    let mut extra: Vec<&String> = limits
        .iter()
        .map(|(a, _)| a)
        .filter(|a| !rows.iter().any(|(r, _, _)| r == *a))
        .collect();
    extra.sort();
    for a in extra {
        out.push((a.clone(), Py::I(0), 0, Py::I(0)));
    }
    let mut json_rows = Vec::new();
    for (a, cost, runs, per_run) in &out {
        let l = lim(a);
        let month = match l {
            Some(_) => rnd(month_spent(c, now, a)?, 4),
            None => Value::Null,
        };
        let at = l
            .and_then(|o| o.get("at_limit"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("ask");
        json_rows.push(json!({
            "agent": a, "cost": cost.value(), "runs": runs, "perRun": per_run.value(),
            "limit": l.and_then(|o| o.get("limit")).cloned().unwrap_or(Value::Null), "at": at, "month": month,
        }));
    }
    let agents_total = out.iter().fold(Py::I(0), |t, (_, cost, _, _)| t.add(*cost));
    let own = total.sub(agents_total).at_least_zero().round(4);
    Ok(format!(
        "{}\n",
        crate::secrets::pyjson::dumps(&json!({
            "period": period, "label": period_label(now, period), "since": since, "prices": "api",
            "total": total.round(4).value(), "agents": agents_total.round(4).value(), "own": own.value(), "rows": json_rows,
        }))
    ))
}

/// `agent NAME --json [PERIOD]`: one agent for the apps.
pub fn agent_json(c: &Connection, p: &Paths, now: &Now, name: &str, period: &str) -> R<String> {
    let since = now.app_since(period);
    let (cost, runs): (Option<f64>, i64) = c.query_row(
        "select sum(cost), count(distinct session) from hq_req where hq_agent = ? and ts >= ?",
        params![name, since],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let cost = Py::or_zero(cost);
    let models: Vec<Value> = {
        let mut st = c.prepare(
            "select model, effort, sum(cost) s, count(distinct session) from hq_req where hq_agent = ? and ts >= ? group by 1, 2 order by s desc limit 6",
        )?;
        st.query_map(params![name, since], |r| {
            Ok((r.get::<_, Option<String>>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<f64>>(2)?, r.get::<_, i64>(3)?))
        })?
        .map(|row| row.map(|(m, e, x, n)| json!({"model": m, "effort": e, "cost": Py::or_zero(x).round(4).value(), "runs": n})))
        .collect::<R<_>>()?
    };
    let costliest: Vec<Value> = {
        let mut st = c.prepare(
            "select session, sum(cost) s, min(datetime(ts,'localtime')), max(via),
               (select turn from req r2 where r2.session = hq_req.session and turn is not null order by ts limit 1)
               from hq_req where hq_agent = ? and ts >= ? group by session order by s desc limit 5",
        )?;
        st.query_map(params![name, since], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, Option<f64>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })?
        .map(|row| row.map(|(sid, x, first, via, turn)| json!({"session": sid, "cost": Py::or_zero(x).round(4).value(), "started": first, "via": via, "turn": turn})))
        .collect::<R<_>>()?
    };
    let limit = limit_of(p, name);
    let spent = month_spent(c, now, name)?;
    let days = month_days(c, now, name)?;
    // The model and effort its newest run used: the lever that matters most.
    let latest: Option<(Option<String>, Option<String>)> = c
        .query_row(
            "select model, effort from runs where agent = ? and model is not null order by started desc limit 1",
            params![name],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let per_run = if runs != 0 {
        Py::F(pyfmt::round(cost.f() / runs as f64, 2))
    } else {
        Py::I(0)
    };
    let out = json!({
        "agent": name, "period": period, "label": period_label(now, period), "prices": "api",
        "cost": cost.round(4).value(), "runs": runs, "perRun": per_run.value(),
        "month": rnd(spent, 4), "monthLabel": period_label(now, "month"),
        "limit": limit.amount.clone().map_or(Value::Null, Value::Number), "at": limit.at,
        "raiseTo": if limit.amount.is_some() { raise_to(limit.value(), spent) } else { Value::Null },
        "days": days.iter().map(|(d, x)| json!({"day": d.format("%Y-%m-%d").to_string(), "cost": rnd(*x, 4)})).collect::<Vec<_>>(),
        "latest": latest.map_or(Value::Null, |(m, e)| json!({"model": m, "effort": e})),
        "models": models, "costliest": costliest,
        "forecast": forecast(now, &days, spent, limit.amount.as_ref()),
    });
    Ok(format!("{}\n", crate::secrets::pyjson::dumps(&out)))
}

/// `<kernel>/bin/kith-auth ARGS`, its output when it succeeds within 20 seconds.
fn kith_auth(p: &Paths, args: &[&str]) -> Option<Value> {
    let mut cmd = Command::new(p.kernel.join("bin").join("kith-auth"));
    cmd.args(args);
    let out = crate::secrets::run_command_with_timeout(cmd, Wait::from_secs(20))?;
    serde_json::from_str(&out).ok()
}

/// `members AGENT`: this calendar month per person on the agent, and each person's budget on it.
pub fn members(c: &Connection, p: &Paths, now: &Now, agent: &str) -> R<String> {
    let mut st = c.prepare(
        "select coalesce(person, 'owner'), sum(cost) from hq_req where hq_agent = ? and ts >= ? group by 1",
    )?;
    let rows: Vec<(String, Option<f64>)> = st
        .query_map(params![agent, Now::utc_text(now.month_start())], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?
        .collect::<R<_>>()?;
    // An unreadable budget is not "nobody has one": the answer says so (`budgetsError`), as the Python does.
    let (budgets, error) = match read_budgets(p, agent) {
        Some(b) => (b, false),
        None => (json!({}), true),
    };
    let people: Map<String, Value> = rows
        .into_iter()
        .map(|(who, x)| (who, Py::or_zero(x).round(4).value()))
        .collect();
    let mut out = json!({"agent": agent, "month": period_label(now, "month"), "people": people, "budgets": budgets});
    if error {
        out["budgetsError"] = json!("can't read budgets");
    }
    Ok(format!("{}\n", crate::secrets::pyjson::dumps(&out)))
}

/// `read_budgets` of `kith-cost`: each person's budget on the agent, from `kith-auth budgets` (which asks the core itself once
/// auth.json is its tombstone). `None` when it can't be read.
fn read_budgets(p: &Paths, agent: &str) -> Option<Value> {
    let mut cmd = Command::new(p.kernel.join("bin").join("kith-auth"));
    cmd.args(["budgets", agent, "--json"]);
    let (code, out) = run_code(cmd, None, Wait::from_secs(20))?;
    if code != 0 {
        return None;
    }
    serde_json::from_str(&out).ok()
}

/// Runs `cmd` with `input` on stdin; its exit code and stdout, `None` when it can't start or outlives `limit` (killed).
fn run_code(mut cmd: Command, input: Option<String>, limit: Wait) -> Option<(i32, String)> {
    use std::io::{Read as _, Write as _};
    use std::process::Stdio;
    let mut child = cmd
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    if let (Some(text), Some(mut si)) = (input, child.stdin.take()) {
        std::thread::spawn(move || {
            let _ = si.write_all(text.as_bytes());
        });
    }
    let mut so = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = so.read_to_string(&mut s);
        s
    });
    let start = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) if start.elapsed() < limit => std::thread::sleep(Wait::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    Some((status.code()?, reader.join().ok()?))
}

/// `bots --json`: the Mac owner's bots this calendar month, all agents, against their cap together. `None` when
/// `kith-auth bots-cap` does not answer (Python lets that raise).
pub fn bots_json(c: &Connection, p: &Paths, now: &Now) -> R<Option<String>> {
    let Some(cap) = kith_auth(p, &["bots-cap", "--json"]) else {
        return Ok(None);
    };
    let ids: Vec<String> = cap
        .get("bots")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|b| b.as_str().map(str::to_owned))
        .collect();
    let rows: Vec<(String, Option<f64>)> = if ids.is_empty() {
        Vec::new()
    } else {
        let marks = vec!["?"; ids.len()].join(",");
        let sql = format!(
            "select person, sum(cost) from hq_req where ts >= ? and person in ({marks}) group by 1"
        );
        let mut st = c.prepare(&sql)?;
        let args = std::iter::once(Now::utc_text(now.month_start())).chain(ids);
        st.query_map(params_from_iter(args), |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<R<_>>()?
    };
    let spent = rows.iter().fold(Py::I(0), |t, (_, x)| {
        t.add(x.filter(|v| *v != 0.0).map_or(Py::I(0), Py::F))
    });
    let bots: Map<String, Value> = rows
        .iter()
        .map(|(who, x)| (who.clone(), Py::or_zero(*x).round(4).value()))
        .collect();
    let out = json!({
        "month": period_label(now, "month"), "cap": cap.get("cap").cloned().unwrap_or(Value::Null),
        "spent": spent.round(4).value(), "bots": bots,
    });
    Ok(Some(format!("{}\n", crate::secrets::pyjson::dumps(&out))))
}
