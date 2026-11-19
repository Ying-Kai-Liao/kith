//! The account set's writers: `kith_settings.py`'s `set_accounts`, `set_cross_harness`, `set_account_policy`, `set_pin`,
//! `clear_pin` and their `_tidy`, as functions on the settings map. Each verb calls them inside one [`edit_with`], so the
//! read, the calculation and the write happen under the one `settings.json.lock` (the Python read the set outside it and
//! could lose a concurrent update).

use super::verbs::Verb;
use super::{Ctx, Map};
use serde_json::{Value, json};

/// `_dedup`: the strings that are not empty, each once, in order.
pub fn dedup(ids: &[String]) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for i in ids {
        if !i.is_empty() && !seen.contains(i) {
            seen.push(i.clone());
        }
    }
    seen
}

/// Runs `f` on the settings under the lock and writes the result. `f` may refuse with a [`Verb`]: nothing is written
/// then. A file that cannot be read or written is exit 1 with its reason, as `kith_settings.edit`'s `ValueError`.
pub fn edit_with<T>(
    ctx: &Ctx<'_>,
    f: impl FnOnce(&mut Map) -> Result<T, Verb>,
) -> Result<T, Verb> {
    let mut refused: Option<Verb> = None;
    let mut out: Option<T> = None;
    let r = crate::settings::edit(ctx, |s| match f(s) {
        Ok(t) => {
            out = Some(t);
            Ok(())
        }
        Err(v) => {
            refused = Some(v);
            Err(String::new())
        }
    });
    if let Some(v) = refused {
        return Err(v);
    }
    r.map_err(|e| Verb::die(&e, 1))?;
    out.ok_or_else(|| Verb::die("settings were not changed", 1))
}

fn agent_entry<'a>(s: &'a mut Map, agent: &str) -> &'a mut Map {
    fn obj(v: &mut Value) -> &mut Map {
        if !v.is_object() {
            *v = json!({});
        }
        match v.as_object_mut() {
            Some(m) => m,
            None => unreachable!("just made an object"),
        }
    }
    let agents = obj(s.entry("agents").or_insert_with(|| json!({})));
    obj(agents.entry(agent).or_insert_with(|| json!({})))
}

/// `_tidy`: an agent entry left empty is dropped.
fn tidy(s: &mut Map, agent: &str) {
    let empty = s
        .get("agents")
        .and_then(|a| a.get(agent))
        .and_then(Value::as_object)
        .is_some_and(Map::is_empty);
    if empty && let Some(Value::Object(a)) = s.get_mut("agents") {
        a.remove(agent);
    }
}

fn forget(s: &mut Map, agent: &str, key: &str) {
    if let Some(Value::Object(a)) = s.get_mut("agents").and_then(|a| a.get_mut(agent)) {
        a.remove(key);
    }
    tidy(s, agent);
}

/// `set_accounts`: one agent's own set, or with no agent the global one; `None` forgets it (an agent then follows the
/// global set).
pub fn set_accounts(s: &mut Map, agent: &str, ids: Option<&[String]>) {
    match (agent.is_empty(), ids) {
        (false, None) => forget(s, agent, "accounts"),
        (false, Some(ids)) => {
            agent_entry(s, agent).insert("accounts".into(), json!(dedup(ids)));
        }
        (true, None) => {
            s.remove("accounts");
        }
        (true, Some(ids)) => {
            s.insert("accounts".into(), json!(dedup(ids)));
        }
    }
}

/// `set_cross_harness`: for one agent (`None` forgets it) or for all.
pub fn set_cross_harness(s: &mut Map, agent: &str, value: Option<bool>) {
    match (agent.is_empty(), value) {
        (false, None) => forget(s, agent, "account_cross_harness"),
        (false, Some(v)) => {
            agent_entry(s, agent).insert("account_cross_harness".into(), json!(v));
        }
        (true, v) => {
            s.insert("account_cross_harness".into(), json!(v.unwrap_or(false)));
        }
    }
}

pub fn set_pin(s: &mut Map, agent: &str, id: &str, only: bool) {
    agent_entry(s, agent).insert("pin".into(), json!({"id": id, "only": only}));
}

pub fn clear_pin(s: &mut Map, agent: &str) {
    forget(s, agent, "pin");
}

/// What `kith account remove` does to the settings: the id leaves the global set, every agent's own list and every pin;
/// an agent entry left empty (or already empty) goes.
pub fn drop_entry(s: &mut Map, entry: &str) {
    let not_entry = |v: &Value| v.as_str() != Some(entry);
    if let Some(Value::Array(a)) = s.get_mut("accounts") {
        a.retain(not_entry);
    }
    if let Some(Value::Object(agents)) = s.get_mut("agents") {
        for a in agents.values_mut() {
            if let Some(a) = a.as_object_mut() {
                if let Some(Value::Array(l)) = a.get_mut("accounts") {
                    l.retain(not_entry);
                }
                if a.get("pin")
                    .and_then(|p| p.get("id"))
                    .and_then(Value::as_str)
                    == Some(entry)
                {
                    a.remove("pin");
                }
            }
        }
        agents.retain(|_, a| a.as_object().is_none_or(|m| !m.is_empty()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(v: Value) -> Map {
        v.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn an_agent_entry_left_empty_is_dropped() {
        let mut s = map(
            json!({"agents": {"a": {"pin": {"id": "claude", "only": false}}, "b": {"accounts": ["codex"]}}}),
        );
        clear_pin(&mut s, "a");
        assert_eq!(
            Value::Object(s.clone()),
            json!({"agents": {"b": {"accounts": ["codex"]}}})
        );
        set_accounts(&mut s, "b", None);
        assert_eq!(Value::Object(s), json!({"agents": {}}));
    }

    #[test]
    fn the_set_keeps_each_id_once_in_order() {
        let mut s = Map::new();
        let ids: Vec<String> = ["claude:a", "", "codex", "claude:a"]
            .map(String::from)
            .to_vec();
        set_accounts(&mut s, "", Some(&ids));
        assert_eq!(Value::Object(s), json!({"accounts": ["claude:a", "codex"]}));
    }

    #[test]
    fn remove_takes_the_id_out_of_every_list_and_pin() {
        let mut s = map(json!({
            "accounts": ["claude", "claude:w"],
            "agents": {"a": {"accounts": ["claude:w"], "pin": {"id": "claude:w", "only": true}},
                       "b": {"pin": {"id": "claude", "only": false}}, "c": {}}
        }));
        drop_entry(&mut s, "claude:w");
        assert_eq!(
            Value::Object(s),
            // `a` keeps its now-empty list (kith-account only drops an agent whose whole entry is `{}`).
            json!({"accounts": ["claude"], "agents": {"a": {"accounts": []}, "b": {"pin": {"id": "claude", "only": false}}}})
        );
    }

    #[test]
    fn cross_harness_is_stored_per_agent_or_for_all() {
        let mut s = Map::new();
        set_cross_harness(&mut s, "a", Some(true));
        set_cross_harness(&mut s, "", Some(false));
        assert_eq!(
            Value::Object(s.clone()),
            json!({"agents": {"a": {"account_cross_harness": true}}, "account_cross_harness": false})
        );
        set_cross_harness(&mut s, "a", None);
        assert_eq!(
            Value::Object(s),
            json!({"agents": {}, "account_cross_harness": false})
        );
    }
}
