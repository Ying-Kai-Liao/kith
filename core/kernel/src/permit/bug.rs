//! `report_bug`: files a bug as a redacted GitHub issue for a guest's agent, whose Bash has no gh login. The report is scrubbed
//! (`kith_redact`), kept under `<state>/bugs/drafts`, shown to the owner on a card (`mcp__kithpermit__report_bug`, the input is
//! the exact draft) and filed only on a clear allow.
//!
//! The scrubbing and the `gh` calls stay `kernel/lib/kith_bug.py`'s (they lean on `kith_redact` and `kith_trajectory`, which have no
//! Rust side, and `kith bug` shares them); this module runs them in two steps around the card, as `file_draft` does in one.

use super::Env;
use super::decide::{allowed, decide};
use super::proc::python;
use super::pyval::strip;
use serde_json::{Map, Value, json};
use std::path::Path;
use std::time::Duration;

/// `kith_bug.CARD_TOOL`: `mcp__` tools run code, so the host always sends their card to the owner.
const CARD_TOOL: &str = "mcp__kithpermit__report_bug";

/// Build the scrubbed draft, keep it, and say whether it was filed before and which open issues look alike
/// (`kith_bug.file_draft` up to the card).
pub(super) const DRAFT: &str = r"
import json, os, sys
sys.path.insert(0, sys.argv[1])
req = json.load(sys.stdin)
try:
    import kith_bug
    env = os.environ
    d = kith_bug.build_draft(req['title'], req['details'], 'agent', env)
    kith_bug.keep(d, env)
    done = kith_bug.filed_url(d, env)
    out = {'draft': d, 'text': kith_bug.draft_text(d)}
    if done:
        out['done'] = done
    else:
        out['card'] = kith_bug.card_input(d, kith_bug.similar(d, env))
    print(json.dumps(out))
except Exception as e:
    print(json.dumps({'err': type(e).__name__}))
";

/// What `file_draft` does after the card: nothing filed without a yes (the draft stays, with the way to file it by hand), else one
/// `gh issue create`, and the issue's address kept beside the draft.
pub(super) const FINISH: &str = r"
import json, os, sys
sys.path.insert(0, sys.argv[1])
req = json.load(sys.stdin)
try:
    import kith_bug
    env = os.environ
    d, yes = req['draft'], req['yes']
    if not yes:
        text = 'Not filed (no yes). Draft kept in bugs/drafts/%s.json. To file it by hand: %s' % (kith_bug.draft_id(d), kith_bug.prefilled_url(d))
    else:
        url, why = kith_bug.create_issue(d, env)
        if url is None:
            text = '%s. Draft kept in bugs/drafts/%s.json. File it by hand: %s' % (why, kith_bug.draft_id(d), kith_bug.prefilled_url(d))
        else:
            f = kith_bug._store(d, env)
            if f is not None and f.exists():
                kith_bug.write_private(f, json.dumps(dict(d, issue=url), indent=1))
            text = 'Filed: %s' % url
    print(json.dumps({'text': text}))
except Exception as e:
    print(json.dumps({'err': type(e).__name__}))
";

const LIMIT: Duration = Duration::from_secs(120);

fn failed(kind: &str) -> String {
    format!("Could not prepare the bug report ({kind}); nothing was filed.")
}

/// The text the agent is told for one `report_bug` call: the redacted draft and what became of it.
pub fn report_bug(a: &Map<String, Value>, env: &Env, kernel: &Path) -> String {
    let title = match a.get("title") {
        Some(Value::String(t)) if !strip(t).is_empty() => t.clone(),
        _ => return "Give a title.".to_owned(),
    };
    let details = a.get("details").and_then(Value::as_str).unwrap_or_default();
    let req = json!({"title": title, "details": details}).to_string();
    let Some(Value::Object(first)) = python(env, kernel, DRAFT, &req, LIMIT) else {
        return failed("RuntimeError");
    };
    if let Some(Value::String(kind)) = first.get("err") {
        return failed(kind);
    }
    let (Some(draft), Some(shown)) = (
        first.get("draft"),
        first.get("text").and_then(Value::as_str),
    ) else {
        return failed("KeyError");
    };
    let final_text = if let Some(Value::String(url)) = first.get("done") {
        format!("Already filed: {url}")
    } else {
        // no way to ask is a no
        let yes = first
            .get("card")
            .is_some_and(|card| allowed(&decide(env, kernel, CARD_TOOL, card)));
        let req = json!({"draft": draft, "yes": yes}).to_string();
        match python(env, kernel, FINISH, &req, LIMIT) {
            Some(Value::Object(m)) => match (m.get("text").and_then(Value::as_str), m.get("err")) {
                (Some(t), _) => t.to_owned(),
                (None, Some(Value::String(kind))) => return failed(kind),
                _ => return failed("KeyError"),
            },
            _ => return failed("RuntimeError"),
        }
    };
    format!("Redacted draft:\n\n{shown}\n\n{final_text}")
}
