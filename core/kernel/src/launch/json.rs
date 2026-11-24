//! The launch object (`--launch-json`): a `Plan` as the one JSON object on stdout.

// The object is written in the order of the bash original, closures and all.
#![allow(clippy::redundant_closure)]

use std::io::Write as _;

use serde_json::{Map, Value, json};

use super::assemble::get;
use super::plan::Plan;
use crate::secrets::pyjson;

/// The launch object's text.
pub fn emit_json(plan: &Plan) -> String {
    let Plan {
        agent: _,
        backup,
        harness,
        claude,
        owner: p_owner,
        person_env_names: p_env_names,
        cfg_var,
        path_names,
        env,
        system_prompt: text,
        out,
        settings: settings_val,
        mcp_servers,
        mcp_arg: _,
        model: _,
        effort: _,
        call_model,
        physical_cwd,
        spec,
        launch: _,
        owner_terminal: _,
        pkg: _,
    } = plan;
    let (claude, p_owner) = (*claude, *p_owner);
    let claude_args: Value = if claude {
        json!(super::args::claude_args(plan))
    } else {
        Value::Null
    };
    let mut names: Vec<String> = path_names.clone();
    names.extend(
        "KITH_ROOT KITH_AGENT KITH_AGENT_DIR KITH_AGENT_HOME KITH_AGENT_VERSION KITH_MODEL KITH_EFFORT KITH_PROJECT KITH_PERSON KITH_RUN_TOKEN KITH_RUN_SOCKET KITH_CORE_RUN_SOCKET KITH_ROOM KITH_ROOM_MEMBERS KITH_ROOM_GEN KITH_ROOM_OWNER_ONLY KITH_COST_PERSON KITH_VIA KITH_PROVIDER KITH_ACCOUNT KITH_ACCOUNT_ID KITH_ACCOUNT_VAR KITH_OWNER CLAUDE_CODE_ARTIFACT"
            .split(' ')
            .map(str::to_owned),
    );
    names.push(cfg_var.clone());
    names.extend(p_env_names.split_whitespace().map(str::to_owned));
    if !p_owner {
        names.extend(
            [
                "CLAUDE_CODE_TMPDIR",
                "KITH_WORK",
                "CLAUDE_ENV_FILE",
                "KITH_BIN",
            ]
            .map(str::to_owned),
        );
    }
    names.extend(
        "HQ_ROOT HQ_AGENT HQ_AGENT_DIR HQ_AGENT_HOME HQ_AGENT_VERSION HQ_MODEL HQ_EFFORT HQ_PROJECT HQ_PERSON HQ_VIA"
            .split(' ')
            .map(str::to_owned),
    );
    let mut env_obj = Map::new();
    for n in names {
        if let Some(v) = env.get(&n) {
            env_obj.entry(n).or_insert(Value::String(v.clone()));
        }
    }
    let nz = |k: &str| -> Value {
        let v = get(env, k);
        if v.is_empty() {
            Value::Null
        } else {
            Value::String(v.to_owned())
        }
    };
    let nzs = |v: &str| -> Value {
        if v.is_empty() {
            Value::Null
        } else {
            Value::String(v.to_owned())
        }
    };
    let mut obj = Map::new();
    let mut put = |k: &str, v: Value| {
        obj.insert(k.to_owned(), v);
    };
    put("version", json!(1));
    put("agent", nz("KITH_AGENT"));
    put("agent_version", nz("KITH_AGENT_VERSION"));
    put("harness", json!(harness));
    put("cwd", json!(physical_cwd));
    put("env", Value::Object(env_obj));
    put("system_prompt", json!(text));
    put("settings", settings_val.clone().unwrap_or(Value::Null));
    put("mcp_servers", mcp_servers.clone());
    put("add_dirs", json!(out.add_dirs));
    put(
        "allowed_tools",
        json!(if claude {
            out.allowed_tools.clone()
        } else {
            vec![]
        }),
    );
    put("disable_slash_commands", json!(out.disable_slash_commands));
    put("model", nz("KITH_MODEL"));
    put("effort", nz("KITH_EFFORT"));
    put("project", nz("KITH_PROJECT"));
    put("call_model", nzs(call_model));
    put(
        "permission_mode",
        out.permission_mode
            .as_deref()
            .map_or(Value::Null, |m| nzs(m)),
    );
    put(
        "cost_tag",
        json!({"agent": get(env, "KITH_AGENT"), "version": get(env, "KITH_AGENT_VERSION"), "model": nz("KITH_MODEL"),
               "effort": nz("KITH_EFFORT"), "via": get(env, "KITH_VIA"), "project": nz("KITH_PROJECT"), "person": nz("KITH_PERSON")}),
    );
    put("claude_args", claude_args);
    put("backup", nzs(backup));
    put("provider", nz("KITH_PROVIDER"));
    put("account", nz("KITH_ACCOUNT"));
    put("account_id", nz("KITH_ACCOUNT_ID"));
    if let Some(sp) = spec {
        for k in ["engine", "acp", "role_prompt", "registry"] {
            put(k, sp.get(k).cloned().unwrap_or(Value::Null));
        }
    }
    let _ = std::io::stdout().flush();
    pyjson::dumps(&Value::Object(obj))
}
