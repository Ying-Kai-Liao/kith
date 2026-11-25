//! The rules over one auth.json snapshot: [`Rules`] answers `may`, `resolve` and the questions the core and the CLI ask.

// The rules use most of the module's items (Ctx, R, die, the table names); the glob is the same one they had inside secrets.
#[allow(clippy::wildcard_imports)]
use super::*;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

/// `bots_of`: the bots a person owns, sorted.
fn bots_of(auth: &Value, person: &str) -> Vec<String> {
    let mut out: Vec<String> = get(auth, "people")
        .as_object()
        .map(|o| {
            o.iter()
                .filter(|(_, v)| {
                    get(v, "bot_of")
                        .as_str()
                        .is_some_and(|b| !b.is_empty() && canonical(auth, b) == person)
                })
                .map(|(b, _)| b.clone())
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// A row of a membership table, or none for a JSON null.
fn not_null(v: &Value) -> Option<&Value> {
    (!v.is_null()).then_some(v)
}

/// `row_of`: the person's membership of agent: their own row, else the agent's `*` row, else none.
pub(crate) fn row_of<'a>(auth: &'a Value, agent: &str, person: &str) -> Option<&'a Value> {
    let rows = get(get(auth, "members"), agent);
    not_null(rows.get(person).unwrap_or_else(|| get(rows, EVERYONE)))
}

fn own_row<'a>(auth: &'a Value, agent: &str, person: &str) -> Option<&'a Value> {
    not_null(get(get(get(auth, "members"), agent), person))
}

/// The person's ticks (`jobs`) on their own row of pkg.
fn ticks<'a>(auth: &'a Value, pkg: &str, person: &str) -> Option<&'a Map<String, Value>> {
    own_row(auth, pkg, person).and_then(|o| get(o, "jobs").as_object())
}

/// The access a membership row holds beyond the box (strings only), as `access_of` builds it.
pub(crate) fn access_of(row: &Value) -> Map<String, Value> {
    let a = get(row, "access");
    ["read", "write", "domains"]
        .into_iter()
        .map(|k| {
            let vs: Vec<Value> = arr(get(a, k))
                .iter()
                .filter(|v| v.is_string())
                .cloned()
                .collect();
            (k.to_owned(), Value::Array(vs))
        })
        .collect()
}

fn free_shell(v: &Value) -> bool {
    v.as_str().is_some_and(|m| FREE_SHELL.contains(&m))
}

/// Whether a helpers list names the agent.
fn calls(helpers: &[Value], agent: &str) -> bool {
    helpers.iter().any(|h| h.as_str() == Some(agent))
}

/// What a run of an agent for a person gets, evaluated over one auth.json snapshot.
pub struct Rules<'a> {
    c: &'a Ctx,
    auth: &'a Value,
    jobs: RefCell<HashMap<String, Rc<Vec<Job>>>>,
    depth: Cell<u32>,
}

impl<'a> Rules<'a> {
    pub fn new(c: &'a Ctx, auth: &'a Value) -> Self {
        Rules {
            c,
            auth,
            jobs: RefCell::default(),
            depth: Cell::new(0),
        }
    }

    fn deeper(&self) -> R<()> {
        self.depth.set(self.depth.get() + 1);
        if self.depth.get() > DEPTH {
            return die("maximum recursion depth exceeded");
        }
        Ok(())
    }

    /// `jobs_of`: the jobs the agent's package declares.
    fn jobs_of(&self, pkg: &str) -> Rc<Vec<Job>> {
        let mut cache = self.jobs.borrow_mut();
        Rc::clone(
            cache
                .entry(pkg.to_owned())
                .or_insert_with(|| Rc::new(self.c.declared_jobs(pkg))),
        )
    }

    /// Whether the agent's package declares a job of this name (capabilities.yaml `jobs:`).
    pub fn declares_job(&self, agent: &str, name: &str) -> bool {
        self.jobs_of(agent).iter().any(|j| j.name == name)
    }

    /// The job of this name the agent's package declares, as it is now (the share sheet's tick keeps its pieces).
    pub fn declared_job(&self, agent: &str, name: &str) -> Option<Job> {
        self.jobs_of(agent).iter().find(|j| j.name == name).cloned()
    }

    /// `acting`: the person this run works for; never the owner inside a guest's sandbox: then it is no one (`-`).
    pub fn acting(&self) -> String {
        let named = self
            .c
            .person
            .clone()
            .filter(|p| !p.is_empty())
            .or_else(|| owner(self.auth))
            .unwrap_or_default();
        let person = canonical(self.auth, &named);
        if is_owner(self.auth, &person) && self.c.in_guest_sandbox() {
            "-".to_owned()
        } else {
            person
        }
    }

    /// `job_grants`: what the person's ticks on their own row of pkg grant, each cut to the package as it is now.
    fn job_grants(&self, pkg: &str, person: &str) -> [Vec<Value>; 3] {
        let mut out: [Vec<Value>; 3] = Default::default();
        if person == EVERYONE {
            return out;
        }
        let Some(ticks) = ticks(self.auth, pkg, person) else {
            return out;
        };
        for (name, tick) in ticks {
            let jobs = self.jobs_of(pkg);
            let granted = effective(tick, jobs.iter().find(|j| j.name == *name));
            for (k, vs) in granted.into_iter().enumerate() {
                for x in vs {
                    if !out[k].contains(&x) {
                        out[k].push(x);
                    }
                }
            }
        }
        out
    }

    /// `helpers_in`: the helpers a run on this membership row (of pkg) may call: the row's list, the person's ticked
    /// jobs' and, with once, an unused "just this once" on their own row that hasn't run out.
    fn helpers_in(&self, pkg: &str, person: &str, row: Option<&Value>, once: bool) -> Vec<Value> {
        let Some(row) = row.filter(|r| r.is_object()) else {
            return Vec::new();
        };
        let mut out: Vec<Value> = arr(get(row, "helpers")).to_vec();
        out.extend(self.job_grants(pkg, person)[1].iter().cloned());
        if once
            && let Some(left) =
                own_row(self.auth, pkg, person).and_then(|o| get(o, "helper_once").as_object())
        {
            for (h, until) in left {
                if until.as_f64().is_some_and(|u| u > self.c.now_ms) {
                    out.push(Value::String(h.clone()));
                }
            }
        }
        dedupe(out)
    }

    /// `may`: (allowed, why). Access is a membership row; a helper is allowed when the caller's membership lists it or a
    /// ticked job of it does (once: or a "just this once" on it, see `helpers_in`).
    pub fn may(
        &self,
        person: &str,
        agent: &str,
        via: Option<&str>,
        once: bool,
    ) -> R<(bool, String)> {
        self.deeper()?;
        let via = via.filter(|v| !v.is_empty());
        let person = canonical(self.auth, person);
        let person = person.as_str();
        if is_owner(self.auth, person) {
            return Ok((true, "owner".to_owned()));
        }
        let entry = get(get(self.auth, "people"), person);
        if !has(get(self.auth, "people"), person) {
            return Ok((
                false,
                format!("'{person}' is not a person in kith (kith-auth people)"),
            ));
        }
        if truthy(get(entry, "revoked")) {
            // before any row, the `*` row included: revoke beats every row, and undo finds them all as they were
            return Ok((false, format!("{person}'s access was removed")));
        }
        if self.c.is_builtin(agent) {
            // before any row, the `*` row and a helper list included: fail closed
            return Ok((
                false,
                self.c.unreadable(agent).unwrap_or_else(|| {
                    format!("{agent} is a built-in agent: only the owner runs it")
                }),
            ));
        }
        if truthy(get(entry, "bot_of")) {
            return self.bot_may(person, agent, via, once);
        }
        if row_of(self.auth, agent, person).is_some() {
            let own = has(get(get(self.auth, "members"), agent), person);
            return Ok((
                true,
                if own { "member" } else { "member (everyone)" }.to_owned(),
            ));
        }
        if let Some(via) = via {
            let helpers = self.helpers_in(via, person, row_of(self.auth, via, person), once);
            if calls(&helpers, agent) {
                return Ok((true, format!("helper of {via}")));
            }
        }
        Ok((
            false,
            format!(
                "{person} may not use {agent}{}",
                via.map_or(String::new(), |v| format!(" (not in their {v} helpers)"))
            ),
        ))
    }

    /// `bot_row`: the bot's own membership row for agent, else the caller's (a helper runs with the caller's row). The
    /// `*` row never reaches a bot: a bot gets an agent only by name.
    fn bot_row(&self, agent: &str, bot: &str, via: Option<&str>, once: bool) -> Option<&'a Value> {
        if let Some(own) = own_row(self.auth, agent, bot) {
            return Some(own);
        }
        let via = via?;
        let theirs = own_row(self.auth, via, bot);
        let helpers = self.helpers_in(via, bot, theirs, once);
        if calls(&helpers, agent) { theirs } else { None }
    }

    /// `bot_may`: a bot may use an agent only with a row of its own AND while its owner may use it right now.
    fn bot_may(&self, bot: &str, agent: &str, via: Option<&str>, once: bool) -> R<(bool, String)> {
        let parent = parent_of(self.auth, bot);
        let Some(parent) = parent.filter(|p| has(get(self.auth, "people"), p)) else {
            let raw = get(get(get(self.auth, "people"), bot), "bot_of")
                .as_str()
                .unwrap_or_default()
                .to_owned();
            return Ok((false, format!("{bot}'s owner '{raw}' is gone")));
        };
        if self.bot_row(agent, bot, via, once).is_none() {
            return Ok((false, format!("{bot} may not use {agent}")));
        }
        if !self.may(&parent, agent, via, false)?.0 {
            return Ok((
                false,
                format!("{bot} may not use {agent}: its owner {parent} may not"),
            ));
        }
        Ok((true, format!("bot of {parent}")))
    }

    /// `preset`: a named bundle, from auth.json or the built-in `guest` and `member`.
    fn preset(&self, name: &Value) -> R<Value> {
        let key = name.as_str().unwrap_or_default();
        let own = get(get(self.auth, "presets"), key);
        if truthy(own) {
            return Ok(own.clone());
        }
        match key {
            "guest" => Ok(serde_json::json!({"mode": "default"})),
            "member" => Ok(serde_json::json!({"mode": "acceptEdits"})),
            _ => die(format!(
                "no preset '{key}' (kith-auth preset {key} mode=..)"
            )),
        }
    }

    /// `agent_domains`: the hosts every guest of `agent` may reach, whoever they are: the ones its package declares
    /// (`guest_domains:` in capabilities.yaml, the owner's consent at install) and the ones the owner kept for everyone
    /// on a card (`agent_access`). Plain host names only.
    pub fn agent_domains(&self, agent: &str) -> Vec<String> {
        let mut all = self.c.declared_domains(agent);
        if let Some(kept) = get(get(get(self.auth, "agent_access"), agent), "domains").as_array() {
            all.extend(kept.iter().filter_map(Value::as_str).map(str::to_owned));
        }
        let mut out: Vec<String> = Vec::new();
        for d in all {
            if jobs::valid_domain(&d) && !out.contains(&d) {
                out.push(d);
            }
        }
        out
    }

    /// `with_jobs`: the run's access with its ticked jobs' domains added, its doors and the names of its ticks, all from
    /// the person's own row of pkg.
    fn with_jobs(
        &self,
        pkg: &str,
        person: &str,
        mut access: Map<String, Value>,
        agent: &str,
    ) -> Map<String, Value> {
        let granted = self.job_grants(pkg, person);
        let mut domains: Vec<Value> = arr(access.get("domains").unwrap_or(&NULL)).to_vec();
        domains.extend(granted[0].iter().cloned());
        domains.extend(self.agent_domains(agent).into_iter().map(Value::String));
        access.insert("domains".to_owned(), Value::Array(dedupe(domains)));
        let mut names: Vec<String> = Vec::new();
        if let Some(ticks) = ticks(self.auth, pkg, person)
            && !ticks.is_empty()
        {
            let jobs = self.jobs_of(pkg);
            names = ticks
                .keys()
                .filter(|n| jobs.iter().any(|j| j.name == **n))
                .cloned()
                .collect();
            names.sort();
        }
        let mut out = Map::new();
        out.insert("access".to_owned(), Value::Object(access));
        out.insert("doors".to_owned(), Value::Array(granted[2].clone()));
        out.insert("jobs".to_owned(), Value::Array(strings(&names)));
        out
    }

    /// `resolve`: what a run of agent for person gets: whether it may run, and the person's settings over the agent's. A
    /// helper reached through the caller's helpers list runs with the caller's membership, so calling one never loosens it.
    pub fn resolve(
        &self,
        agent: &str,
        person: &str,
        via: Option<&str>,
        once: bool,
    ) -> R<Map<String, Value>> {
        self.deeper()?;
        let via = via.filter(|v| !v.is_empty());
        let person = canonical(self.auth, person);
        let person = person.as_str();
        let (ok, why) = self.may(person, agent, via, once)?;
        let me = get(get(self.auth, "people"), person);
        let mut out = Map::new();
        out.insert("person".to_owned(), person.into());
        out.insert("owner".to_owned(), is_owner(self.auth, person).into());
        out.insert("allowed".to_owned(), ok.into());
        out.insert("why".to_owned(), why.into());
        for k in ["name", "lang", "tz"] {
            if let Some(v) = me.get(k) {
                out.insert(k.to_owned(), v.clone());
            }
        }
        if !ok || out["owner"] == Value::Bool(true) {
            return Ok(out);
        }
        if truthy(get(me, "bot_of")) {
            return self.resolve_bot(agent, person, via, out, once);
        }
        let (row, pkg) = match row_of(self.auth, agent, person) {
            Some(r) => (r, agent),
            None => match via.and_then(|v| row_of(self.auth, v, person).map(|r| (r, v))) {
                Some(found) => found,
                None => return die(format!("{person} has no membership of {agent}")),
            },
        };
        let p = self.preset(&get_or(row, "preset", "guest".into()))?;
        let mode = {
            let asked = get(row, "mode");
            let mode = if truthy(asked) {
                asked.clone()
            } else {
                get_or(&p, "mode", "default".into())
            };
            let trusted = if truthy(asked) {
                get(row, "trusted")
            } else {
                get(&p, "trusted")
            };
            if free_shell(&mode) && !truthy(trusted) {
                Value::from("default")
            } else {
                mode
            }
        };
        let deny = dedupe([arr(get(&p, "deny")), arr(get(row, "deny"))].concat());
        let row_agent = row_of(self.auth, agent, person).is_some();
        out.insert("preset".to_owned(), get_or(row, "preset", "guest".into()));
        out.insert("mode".to_owned(), mode.clone());
        out.insert("deny".to_owned(), Value::Array(deny));
        out.insert("model".to_owned(), get_or(row, "model", "".into()));
        out.insert(
            "env".to_owned(),
            get_or(row, "env", Value::Object(Map::new())),
        );
        out.insert(
            "helpers".to_owned(),
            Value::Array(self.helpers_in(pkg, person, Some(row), false)),
        );
        out.insert("budget".to_owned(), get_or(row, "budget", "".into()));
        out.insert(
            "budget_agent".to_owned(),
            if row_agent {
                agent.into()
            } else {
                via.map_or(Value::Null, Value::from)
            },
        );
        out.insert(
            "was".to_owned(),
            get_or(me, "was", Value::Array(Vec::new())),
        );
        out.insert(
            "trusted".to_owned(),
            (truthy(get(row, "trusted")) || truthy(get(&p, "trusted"))).into(),
        );
        out.extend(self.with_jobs(pkg, person, access_of(row), agent));
        let asked = if truthy(get(row, "mode")) {
            get(row, "mode")
        } else {
            get(&p, "mode")
        };
        if free_shell(asked) && &mode != asked {
            out.insert("downgraded".to_owned(), asked.clone());
        }
        Ok(out)
    }

    /// `resolve_bot`: a bot's run: a guest, inside what its owner gets. It spends from its owner's budget, counted with
    /// everything else the owner's line holds (`was`).
    fn resolve_bot(
        &self,
        agent: &str,
        bot: &str,
        via: Option<&str>,
        mut out: Map<String, Value>,
        once: bool,
    ) -> R<Map<String, Value>> {
        let Some(row) = self.bot_row(agent, bot, via, once) else {
            return die(format!("{bot} has no membership of {agent}"));
        };
        let pkg = if own_row(self.auth, agent, bot).is_some() {
            agent
        } else {
            via.unwrap_or_default()
        };
        let parent = parent_of(self.auth, bot).unwrap_or_default();
        let pr = self.resolve(agent, &parent, via, false)?;
        let p = self.preset(&"guest".into())?;
        let owner_parent = is_owner(self.auth, &parent);
        let mut helpers = self.helpers_in(pkg, bot, Some(row), false);
        if !owner_parent {
            let theirs = arr(pr.get("helpers").unwrap_or(&NULL)).to_vec();
            helpers.retain(|h| theirs.contains(h));
        }
        let mut was: Vec<Value> = Vec::new();
        for b in bots_of(self.auth, &parent).into_iter().filter(|b| b != bot) {
            was.push(Value::String(b.clone()));
            was.extend(
                arr(get(get(get(self.auth, "people"), &b), "was"))
                    .iter()
                    .cloned(),
            );
        }
        let (budget, budget_agent) = if owner_parent {
            // the owner's own runs are uncapped; the optional cap covers their bots together and leaves out their own runs
            let cap = get(self.auth, "owner_bots_budget");
            (
                if truthy(cap) { cap.clone() } else { "".into() },
                Value::from(""),
            )
        } else {
            let mut all: Vec<Value> = vec![Value::String(parent.clone())];
            all.extend(
                arr(get(get(get(self.auth, "people"), &parent), "was"))
                    .iter()
                    .cloned(),
            );
            all.extend(arr(pr.get("was").unwrap_or(&NULL)).iter().cloned());
            all.extend(was);
            was = all;
            (
                pr.get("budget").cloned().unwrap_or_else(|| "".into()),
                pr.get("budget_agent").cloned().unwrap_or_else(|| "".into()),
            )
        };
        let mode = {
            let m = get(&p, "mode");
            if free_shell(m) && !truthy(get(&p, "trusted")) {
                Value::from("default")
            } else {
                get_or(&p, "mode", "default".into())
            }
        };
        let deny = dedupe(
            [
                arr(get(&p, "deny")),
                arr(get(row, "deny")),
                arr(pr.get("deny").unwrap_or(&NULL)),
            ]
            .concat(),
        );
        let model = if truthy(get(row, "model")) {
            get(row, "model").clone()
        } else {
            pr.get("model").cloned().unwrap_or_else(|| "".into())
        };
        out.insert("preset".to_owned(), "guest".into());
        out.insert("mode".to_owned(), mode);
        out.insert("deny".to_owned(), Value::Array(deny));
        out.insert("model".to_owned(), model);
        out.insert(
            "env".to_owned(),
            get_or(row, "env", Value::Object(Map::new())),
        );
        out.insert("helpers".to_owned(), Value::Array(helpers));
        out.insert("budget".to_owned(), budget);
        out.insert("budget_agent".to_owned(), budget_agent);
        out.insert("was".to_owned(), Value::Array(dedupe(was)));
        let grants = self.with_jobs(pkg, bot, access_of(row), agent);
        out.extend(self.owner_cut(agent, bot, grants, via, Some(&pr))?);
        Ok(out)
    }

    /// `owner_cut`: a bot's grants (any of access, doors, jobs) never more than its owner holds: each cut to what the
    /// owner's run of agent (through via) has. The owner's own bots are capped by nothing.
    fn owner_cut(
        &self,
        agent: &str,
        bot: &str,
        grants: Map<String, Value>,
        via: Option<&str>,
        pr: Option<&Map<String, Value>>,
    ) -> R<Map<String, Value>> {
        let parent = parent_of(self.auth, bot).unwrap_or_default();
        if is_owner(self.auth, &parent) {
            return Ok(grants);
        }
        let own;
        let pr = if let Some(p) = pr {
            p
        } else {
            own = self.resolve(agent, &parent, via, false)?;
            &own
        };
        let mut out = grants;
        if let Some(access) = out.get("access").and_then(Value::as_object) {
            let theirs = pr.get("access").unwrap_or(&NULL);
            let cut: Map<String, Value> = access
                .iter()
                .map(|(k, vs)| {
                    (
                        k.clone(),
                        Value::Array(
                            arr(vs)
                                .iter()
                                .filter(|v| held(theirs, k, v))
                                .cloned()
                                .collect(),
                        ),
                    )
                })
                .collect();
            out.insert("access".to_owned(), Value::Object(cut));
        }
        for k in ["doors", "jobs"] {
            if let Some(vs) = out.get(k) {
                let theirs = arr(pr.get(k).unwrap_or(&NULL));
                let kept: Vec<Value> = arr(vs)
                    .iter()
                    .filter(|x| theirs.contains(x))
                    .cloned()
                    .collect();
                out.insert(k.to_owned(), Value::Array(kept));
            }
        }
        Ok(out)
    }

    /// `kith-auth who <platform> <user-id>` (M2-23): which person owns a chat handle, with the owner, whether the owner is
    /// linked on that platform, and the revoke and held flags the channels gate on.
    pub fn who(&self, platform: &str, uid: &str) -> R<Value> {
        // A database imported before `people.handles` existed has none for anyone: a chat sender would match nobody (or the
        // wrong person), so it says so and refuses until the import is run again.
        if truthy(get(self.auth, "handles_stale")) {
            return die(
                "cannot match a chat handle to a person: this database was imported before people.handles existed; import again first",
            );
        }
        let people = get(self.auth, "people");
        let pid = people.as_object().and_then(|o| {
            o.iter()
                .find(|(_, v)| get(get(v, "handles"), platform).as_str() == Some(uid))
                .map(|(p, _)| p.clone())
        });
        let me = owner(self.auth);
        let flag = |f: &dyn Fn(&str) -> bool| pid.as_deref().is_some_and(f);
        let mut out = Map::new();
        out.insert(
            "person".into(),
            pid.clone().map_or(Value::Null, Value::String),
        );
        out.insert("owner".into(), flag(&|p| is_owner(self.auth, p)).into());
        out.insert(
            "was".into(),
            pid.as_deref().map_or_else(
                || Value::Array(Vec::new()),
                |p| get_or(get(people, p), "was", Value::Array(Vec::new())),
            ),
        );
        out.insert(
            "owner_id".into(),
            me.clone().map_or(Value::Null, Value::String),
        );
        out.insert(
            "owner_linked".into(),
            me.as_deref()
                .is_some_and(|m| truthy(get(get(get(people, m), "handles"), platform)))
                .into(),
        );
        out.insert("revoked".into(), flag(&|p| cut_off(self.auth, p)).into());
        out.insert("held".into(), flag(&|p| self.bridge_held(p)).into());
        Ok(Value::Object(out))
    }

    /// `kith-auth granted [--for AGENT] <service>`: the profiles an agent holds for a service, as written (one name or a
    /// list).
    pub fn granted(&self, agent: &str, service: &str) -> R<Vec<Value>> {
        // `grants` is filled by the authority import; a database imported before it did has none, which says nothing.
        if truthy(get(self.auth, "secrets_stale")) {
            return die(
                "cannot read the grants: this database was imported before they were kept; import again first",
            );
        }
        let g = get(get(get(self.auth, "grants"), agent), service);
        Ok(match g {
            Value::Array(items) => items.clone(),
            g if truthy(g) => vec![g.clone()],
            _ => Vec::new(),
        })
    }

    /// `cloud_sync` and `bridge_held`: kith serve's channels serve a person other than the owner only while this Mac is
    /// in sync with the Kith cloud; a linked Mac whose host hasn't renewed the lease lately holds them.
    pub fn bridge_held(&self, person: &str) -> bool {
        if person.is_empty() || is_owner(self.auth, person) {
            return false;
        }
        let Ok(text) = std::fs::read_to_string(self.c.serve_dir().join("cloud-sync.json")) else {
            return false;
        };
        let Ok(Value::Object(s)) = serde_json::from_str::<Value>(&text) else {
            return false;
        };
        let linked = s.get("linked").is_some_and(truthy);
        let until = match s.get("until") {
            None | Some(Value::Null | Value::Bool(false)) => Some(0.0),
            Some(Value::Bool(true)) => Some(1.0),
            Some(Value::Number(n)) => n.as_f64(),
            Some(Value::String(t)) if t.is_empty() => Some(0.0),
            Some(Value::String(t)) => t.trim().parse::<f64>().ok(),
            Some(other) => (!truthy(other)).then_some(0.0),
        };
        // a value Python's `float()` refuses reads as "never linked"
        until.is_some_and(|u| linked && u <= self.c.now_ms)
    }
}
