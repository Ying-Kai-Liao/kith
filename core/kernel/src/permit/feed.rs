//! `feed_post`: one card on the owner's Feed, from an owner run. The MCP tool (`mcp.rs`) and the `feed-post` verb (what
//! `kith feed post` runs, so a tool such as kith-merge can post as a child of an owner run) are the same call: one
//! `feedPost` line on the owner's socket (`KITH_ASK_TOKEN`, `KITH_ASK_SOCKET`), never the guest's. The host judges it:
//! the token's scope, the author, the chat and the daily cap are its, so nothing here is authority.

use super::access::said;
use super::host::call_host_ask;
use super::{Env, Out};
use serde_json::{Map, Value, json};
use std::time::Duration;

/// A post may copy a file of 100 MiB; the host answers when it is done.
const CALL_S: u64 = 60;

/// The tool's arguments as the host's line (`kind`, `title`, `line`, `file`, `url`, `number`, `fold`, `pr`), or what is wrong
/// with them. A relative `file` is made absolute here, from the run's folder: the host does not know it.
pub fn body(a: &Map<String, Value>) -> Result<Map<String, Value>, String> {
    let text = |k: &str| match a.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(Some(s.trim().to_owned())),
        _ => Err(format!("{k} is text")),
    };
    let (Some(kind), Some(title)) = (text("kind")?, text("title")?) else {
        return Err("Give the post a kind and a title.".to_owned());
    };
    let mut out = Map::new();
    out.insert("kind".to_owned(), kind.into());
    out.insert("title".to_owned(), title.into());
    for k in ["line", "url", "fold", "pr"] {
        if let Some(v) = text(k)? {
            out.insert(k.to_owned(), v.into());
        }
    }
    let number = a.get("number").filter(|n| !n.is_null());
    let file = text("file")?;
    if usize::from(file.is_some())
        + usize::from(out.contains_key("url"))
        + usize::from(number.is_some())
        > 1
    {
        return Err("Give at most one of file, url and number.".to_owned());
    }
    if let Some(f) = file {
        let path = std::path::Path::new(&f);
        let abs = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map_err(|e| format!("no folder to find {f} in: {e}"))?
                .join(path)
        };
        out.insert("file".to_owned(), abs.to_string_lossy().into_owned().into());
    }
    if let Some(n) = number {
        let n = n
            .as_object()
            .filter(|n| n.get("value").is_some_and(Value::is_number));
        let Some(n) = n else {
            return Err(
                "number is {value, unit?, delta?} with a value that is a number.".to_owned(),
            );
        };
        let keep: Map<String, Value> = ["value", "unit", "delta"]
            .iter()
            .filter_map(|k| Some(((*k).to_owned(), n.get(*k)?.clone())))
            .collect();
        out.insert("number".to_owned(), Value::Object(keep));
    }
    Ok(out)
}

/// The host's reply, or why there is none.
fn ask(a: &Map<String, Value>, env: &Env) -> Result<Map<String, Value>, String> {
    let body = body(a)?;
    call_host_ask(env, "feedPost", &body, Duration::from_secs(CALL_S)).ok_or_else(|| {
        "No Feed for this run (it has no owner token): nothing was posted. Say what you would have posted and carry on.".to_owned()
    })
}

/// The text the agent is told for one `feed_post` call.
pub fn feed_post(a: &Map<String, Value>, env: &Env) -> String {
    match ask(a, env) {
        Ok(reply) => said(&reply, "The Feed gave no clear answer; nothing was posted."),
        Err(why) => why,
    }
}

/// `feed-post --kind K --title T [--line L] [--file P | --url U | --number V [--unit U] [--delta D]] [--fold F] [--pr R] [--json]`:
/// prints the host's answer (with `--json`, its reply line), exits 1 when nothing was posted.
pub fn cli(args: &[&str], env: &Env) -> Out {
    let (mut a, mut json_out) = (Map::new(), false);
    let mut number = Map::new();
    let mut it = args.iter();
    while let Some(flag) = it.next() {
        if *flag == "--json" {
            json_out = true;
            continue;
        }
        let Some(name) = flag.strip_prefix("--") else {
            return usage(&format!("unexpected {flag}"));
        };
        let Some(value) = it.next() else {
            return usage(&format!("{flag} needs a value"));
        };
        match name {
            "kind" | "title" | "line" | "file" | "url" | "fold" | "pr" => {
                a.insert(name.to_owned(), json!(value));
            }
            "number" | "delta" => match value.parse::<f64>() {
                Ok(v) if v.is_finite() => {
                    number.insert(
                        if name == "number" { "value" } else { name }.to_owned(),
                        json!(v),
                    );
                }
                _ => return usage(&format!("{flag} is a number")),
            },
            "unit" => {
                number.insert("unit".to_owned(), json!(value));
            }
            _ => return usage(&format!("unknown {flag}")),
        }
    }
    if !number.is_empty() {
        a.insert("number".to_owned(), Value::Object(number));
    }
    let (ok, shown) = match ask(&a, env) {
        Ok(reply) => {
            let ok = reply.get("ok") == Some(&Value::Bool(true));
            let shown = if json_out {
                crate::secrets::pyjson::dumps(&Value::Object(reply))
            } else {
                said(&reply, "The Feed gave no clear answer; nothing was posted.")
            };
            (ok, shown)
        }
        Err(why) if json_out => (false, json!({"ok": false, "error": why}).to_string()),
        Err(why) => (false, why),
    };
    Out {
        stdout: format!("{shown}\n"),
        stderr: String::new(),
        code: i32::from(!ok),
    }
}

fn usage(why: &str) -> Out {
    Out {
        stdout: String::new(),
        stderr: format!(
            "kith feed post: {why}\nusage: kith feed post --kind K --title T [--line L] [--file PATH | --url https://... | --number V \
             [--unit U] [--delta D]] [--fold NAME] [--pr owner/repo#N] [--json]\n"
        ),
        code: 2,
    }
}
