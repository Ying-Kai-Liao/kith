//! The Plan card: an agent's plan (Claude's todo list, an ACP `plan` update) as one report card that changes in place
//! while the run works through it. The host keeps one card per turn, rewritten on every plan update; steps are
//! done, now or next, and when the turn ends with the plan unfinished nothing is left spinning: the step it was on
//! reads as stopped.
use serde_json::{Value, json};

/// Steps shown and the length of a step's words; a plan longer than this is cut, the count still says how long it was.
const MAX_STEPS: usize = 40;
const MAX_WORDS: usize = 200;

fn state(status: &str, ended: bool) -> &'static str {
    match status {
        "completed" => "done",
        "in_progress" if ended => "stopped",
        "in_progress" => "now",
        _ => "next",
    }
}

fn mark(state: &str) -> &'static str {
    match state {
        "done" => "✓",
        "now" => "→",
        "stopped" => "–",
        _ => "○",
    }
}

/// The card's entry data (`text`, `style`, `richCard`) for the ACP plan `entries`, or `None` for a plan with no steps
/// (nothing to show yet). `ended` is the turn being over: a step still in progress is then stopped.
pub fn card(entries: &Value, entry_id: &str, ended: bool) -> Option<Value> {
    let all = entries.as_array().filter(|e| !e.is_empty())?;
    let steps: Vec<Value> = all
        .iter()
        .take(MAX_STEPS)
        .map(|e| {
            let text: String = e["content"].as_str().unwrap_or_default().trim().chars().take(MAX_WORDS).collect();
            json!({"text": text, "state": state(e["status"].as_str().unwrap_or_default(), ended)})
        })
        .collect();
    let done = all.iter().filter(|e| e["status"] == "completed").count();
    let total = all.len();
    let mut lines = vec![format!("Plan · {done} of {total}")];
    for (i, step) in steps.iter().enumerate() {
        let (words, state) = (step["text"].as_str().unwrap_or_default(), step["state"].as_str().unwrap_or_default());
        lines.push(format!("{}. {} {words}", i + 1, mark(state)));
    }
    if total > steps.len() {
        lines.push(format!("… and {} more", total - steps.len()));
    }
    let text = lines.join("\n");
    Some(json!({
        "text": text,
        "style": "info",
        "richCard": {
            "type": "plan",
            "v": 1,
            "ref": {"entry": entry_id},
            "snapshot": {"title": "Plan", "done": done, "total": total, "ended": ended, "steps": steps},
        },
    }))
}
