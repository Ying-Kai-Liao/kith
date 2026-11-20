//! An agent row never holds an empty agent name (one-bot-model C5): old rows load, and an empty name is a native row.

use super::*;
use serde_json::{Value, json};

fn load(extra: &Value) -> BotConfig {
    let mut v = json!({"id": "a", "name": "A", "backend": "claude", "cwd": "/"});
    v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    serde_json::from_value(v).unwrap()
}

#[test]
fn old_rows_load_in_each_shape() {
    let named = load(&json!({"hqAgent": "house"}));
    assert_eq!(named.hq_agent.as_deref(), Some("house"));
    assert!(named.is_hq() && !named.is_native());

    for raw in [json!({"hqAgent": ""}), json!({"hqAgent": "   "}), json!({"hqAgent": null}), json!({})] {
        let row = load(&raw);
        assert!(row.hq_agent.is_none(), "{raw} holds no agent name");
        assert!(row.is_native() && !row.is_hq(), "{raw} is a native row");
    }

    let room = load(&json!({"kind": "group", "members": []}));
    assert!(room.hq_agent.is_none() && !room.is_native() && !room.is_hq() && room.is_group());
}

#[test]
fn a_name_serializes_as_a_plain_string_and_an_empty_one_is_refused() {
    assert_eq!(serde_json::to_value(load(&json!({"hqAgent": "house"}))).unwrap()["hqAgent"], "house");
    assert!(serde_json::to_value(load(&json!({"hqAgent": ""}))).unwrap().get("hqAgent").is_none());
    assert!(AgentName::new("").is_err());
    assert!(AgentName::new(" \t").is_err());
    assert_eq!(AgentName::new("house").unwrap().as_str(), "house");
}
