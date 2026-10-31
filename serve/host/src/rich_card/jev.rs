//! Jev reply cards (DESIGN.md "Reply cards", step 8): behind an owner opt-in, a finished reply that reads like a result
//! (a lookup, a status report, a table of figures) gets a Result card attached to it. The reply stays: `data.text` is the
//! card's twin, and the apps fold the card back to it ("Show as text").
//!
//! Jev only picks. Code splits the reply into numbered blocks; Jev says whether the reply is a result and which block
//! numbers fill the title, the facts and the links; every string on the card is copied from those blocks (markdown
//! marks stripped), so Jev never writes a word. Anything short of a clear answer leaves the reply plain, and Jev being
//! missing or down is never shown to anyone. Asking cards (Key, Access, permission) never come from here.
//!
//! Method and thresholds from the lab's `jev-reply-cards` experiment (wording v2, kind `info` at 0.85, slots at 0.5).

use crate::chat::router::Jev;
use crate::hub::Hub;
use crate::store::Store;
use serde_json::{Map, Value, json};
use std::sync::Arc;
use std::time::Duration;

/// The owner's switch (`kv`, not a table): absent is off.
const SETTING: &str = "jev.cards";

/// Replies shorter than this stay plain: there is nothing to sort.
const MIN_CHARS: usize = 150;
/// Blocks past these are neither sent to Jev nor eligible: a reply this long is not a card.
const MAX_BLOCKS: usize = 60;
/// Each block's text as Jev sees it.
const SENT_CHARS: usize = 600;
/// A title or fact longer than this reads as prose, not a card line.
const TITLE_CHARS: usize = 100;
const FACT_CHARS: usize = 80;
/// Lab thresholds: the reply is a result at this probability, a block fills a slot at this score.
const KIND_MIN: f64 = 0.85;
const TITLE_MIN: f64 = 0.6;
const SLOT_MIN: f64 = 0.5;
const MAX_FACTS: usize = 4;
const MAX_LINKS: usize = 2;
/// Everything Jev-related, key lookup included, ends here.
const OVERALL: Duration = Duration::from_secs(12);

pub fn enabled(store: &Store) -> bool {
    store.kv_read(SETTING).ok().flatten().is_some_and(|v| v == "on")
}

/// `{enabled}`; the Jev key is the router's (`routerStatus` says whether it is saved).
pub fn status(store: &Store) -> Value {
    json!({"enabled": enabled(store)})
}

pub fn set_enabled(store: &Store, on: bool) -> anyhow::Result<Value> {
    if on {
        store.kv_set(SETTING, "on")?;
    } else {
        store.kv_delete(SETTING)?;
    }
    Ok(status(store))
}

// ---- blocks ----

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Heading,
    Item,
    Row,
    Quote,
    Code,
    Kv,
    Para,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Heading => "heading",
            Self::Item => "item",
            Self::Row => "row",
            Self::Quote => "quote",
            Self::Code => "code",
            Self::Kv => "kv",
            Self::Para => "para",
        }
    }
}

/// A piece of the reply. `i` starts at 1: it is what Jev's answers refer to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Block {
    pub i: usize,
    pub kind: Kind,
    pub text: String,
}

fn list_item(line: &str) -> Option<(usize, &str)> {
    let rest = line.trim_start();
    let indent = line[..line.len() - rest.len()].replace('\t', "  ").chars().count();
    let after = if let Some(r) = rest.strip_prefix(['-', '*', '+']) {
        r
    } else {
        let digits = rest.find(|c: char| !c.is_ascii_digit()).filter(|d| *d > 0)?;
        rest[digits..].strip_prefix(['.', ')'])?
    };
    after.starts_with(char::is_whitespace).then(|| (indent / 2, after.trim()))
}

fn heading(line: &str) -> Option<&str> {
    let rest = line.trim_start();
    let hashes = rest.chars().take_while(|c| *c == '#').count();
    let text = rest[hashes..].strip_prefix(char::is_whitespace)?;
    (1..=6).contains(&hashes).then_some(text.trim())
}

/// A line that is only `**text**` or `**text**:`.
fn bold_line(line: &str) -> Option<&str> {
    let inner = line.trim().strip_prefix("**")?;
    let inner = inner.strip_suffix(':').unwrap_or(inner).strip_suffix("**")?;
    (!inner.is_empty() && inner.chars().count() <= 120 && !inner.contains('*')).then_some(inner.trim())
}

/// `Key: value`, the key a short capitalised label (optionally bold).
fn key_value(line: &str) -> bool {
    let s = line.trim_start();
    let s = s.strip_prefix("**").unwrap_or(s);
    let Some(colon) = s.char_indices().take(40).find(|(_, c)| *c == ':').map(|(i, _)| i) else { return false };
    let key = s[..colon].trim_end_matches('*');
    let mut chars = key.chars();
    let label = chars.next().is_some_and(|c| c.is_ascii_uppercase())
        && key.chars().count() <= 31
        && key.chars().all(|c| c.is_alphanumeric() || " _/&'-".contains(c));
    let value = s[colon + 1..].trim_start_matches('*');
    label && value.starts_with(char::is_whitespace) && !value.trim().is_empty()
}

fn table_separator(line: &str) -> bool {
    let s = line.trim().trim_start_matches('|').trim_start();
    let s = s.strip_prefix(':').unwrap_or(s);
    s.starts_with("--")
}

fn add(blocks: &mut Vec<Block>, kind: Kind, text: &str) {
    let text = text.trim();
    if !text.is_empty() {
        blocks.push(Block { i: blocks.len() + 1, kind, text: text.to_owned() });
    }
}

fn flush(blocks: &mut Vec<Block>, para: &mut Vec<&str>) {
    if !para.is_empty() {
        add(blocks, Kind::Para, &para.iter().map(|l| l.trim()).collect::<Vec<_>>().join(" "));
        para.clear();
    }
}

/// The reply as numbered blocks: headings, list items (one each), table rows, quotes, code, `Key: value` lines, and
/// paragraphs.
pub fn split(text: &str) -> Vec<Block> {
    let mut blocks: Vec<Block> = Vec::new();
    let mut para: Vec<&str> = Vec::new();
    let mut fence: Option<Vec<&str>> = None;
    let text = text.replace("\r\n", "\n");
    for line in text.split('\n') {
        if let Some(code) = fence.as_mut() {
            if line.trim().starts_with("```") {
                let body = code.join("\n");
                add(&mut blocks, Kind::Code, &body);
                fence = None;
            } else {
                code.push(line);
            }
            continue;
        }
        if line.trim().starts_with("```") {
            flush(&mut blocks, &mut para);
            fence = Some(Vec::new());
        } else if line.trim().is_empty() {
            flush(&mut blocks, &mut para);
        } else if let Some(h) = heading(line).or_else(|| bold_line(line)) {
            flush(&mut blocks, &mut para);
            add(&mut blocks, Kind::Heading, h);
        } else if line.trim_start().starts_with('|') {
            flush(&mut blocks, &mut para);
            if !table_separator(line) {
                let cells: Vec<&str> = line.trim().trim_matches('|').split('|').map(str::trim).collect();
                add(&mut blocks, Kind::Row, &cells.join(" | "));
            }
        } else if let Some(quote) = line.trim_start().strip_prefix('>') {
            flush(&mut blocks, &mut para);
            add(&mut blocks, Kind::Quote, quote);
        } else if let Some((_, item)) = list_item(line) {
            flush(&mut blocks, &mut para);
            add(&mut blocks, Kind::Item, item);
        } else if para.is_empty()
            && blocks.last().is_some_and(|b| b.kind == Kind::Item)
            && line.starts_with([' ', '\t'])
        {
            // A list item's continuation line.
            if let Some(last) = blocks.last_mut() {
                last.text.push(' ');
                last.text.push_str(line.trim());
            }
        } else if para.is_empty() && key_value(line) {
            add(&mut blocks, Kind::Kv, line);
        } else {
            para.push(line);
        }
    }
    if let Some(code) = fence {
        add(&mut blocks, Kind::Code, &code.join("\n"));
    }
    flush(&mut blocks, &mut para);
    blocks
}

/// Whether the reply is worth a Jev call: long enough, with more than a single block (a card needs a title and a
/// fact). The lab found a structural prefilter lost real cards; this one is about latency and what leaves the Mac.
pub fn prefilter(text: &str, blocks: &[Block]) -> bool {
    text.chars().count() >= MIN_CHARS && blocks.len() >= 2
}

/// The web links in `text`, in order.
fn urls(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(at) = ["https://", "http://"].iter().filter_map(|p| rest.find(p)).min() {
        let tail = &rest[at..];
        let end = tail.find(|c: char| c.is_whitespace() || ")>]\"'<".contains(c)).unwrap_or(tail.len());
        let url = tail[..end].trim_end_matches(['.', ',', ';', ':', '!', '?']);
        if url.len() > "https://".len() && url.len() <= 500 {
            found.push(url.to_owned());
        }
        rest = &tail[end..];
    }
    found
}

/// Markdown marks off a line: `**bold**`, `` `code` `` and `[label](url)` reads as its label. Only deletes.
fn plain(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let linked = rest[open..].find("](").and_then(|mid| {
            let close = rest[open + mid..].find(')')?;
            Some((open + mid, open + mid + close))
        });
        match linked {
            Some((mid, close)) if mid > open + 1 => {
                out.push_str(&rest[..open]);
                out.push_str(&rest[open + 1..mid]);
                rest = &rest[close + 1..];
            }
            _ => {
                out.push_str(&rest[..=open]);
                rest = &rest[open + 1..];
            }
        }
    }
    out.push_str(rest);
    out.replace("**", "").replace("__", "").replace('`', "").trim().to_owned()
}

/// A link's button label: the markdown label the reply gave it when short, else a plain "Open".
fn link_label(text: &str, url: &str) -> String {
    let label = text.find(&format!("]({url}")).and_then(|mid| {
        let open = text[..mid].rfind('[')?;
        Some(text[open + 1..mid].trim())
    });
    label.filter(|l| !l.is_empty() && l.chars().count() <= 24 && !l.starts_with("http")).unwrap_or("Open").to_owned()
}

// ---- asking Jev ----

const KIND_QUESTION: &str = "What kind of message is `reply`, taken as a whole?";
const FACT_QUESTION: &str =
    "Does block {n} of `reply` state a figure, a status or a named value about the reply's subject?";
const LINK_QUESTION: &str = "Is block {n} of `reply` a link that the reader would open to see the result itself?";
const TITLE_QUESTION: &str = "Which block of `reply` best names what the reply is about, as a title for it?";

fn eligible(b: &Block, max: usize) -> bool {
    b.kind != Kind::Code && b.i <= MAX_BLOCKS && plain(&b.text).chars().count() <= max
}

/// What Jev is asked: what the reply is (the lab's four-way wording, so a digest or a draft isn't forced into
/// "result"), then, per block, the slot questions.
pub fn ask_for(blocks: &[Block]) -> (Value, Map<String, Value>) {
    let shown = &blocks[..blocks.len().min(MAX_BLOCKS)];
    let state = json!({"reply": shown.iter().map(|b| json!({
        "block": b.i,
        "kind": b.kind.name(),
        "text": b.text.chars().take(SENT_CHARS).collect::<String>(),
    })).collect::<Vec<_>>()});
    let mut q = Map::new();
    q.insert(
        "kind".into(),
        json!({"type": "choice", "instructions": KIND_QUESTION, "criteria": {
            "digest": "A brief or digest: it passes on several separate news items, posts, events, opportunities or links for the reader to look at, even when it also says what the assistant did to collect them.",
            "info": "A status report or lookup: it reports measured figures or the current status of one thing, such as a disk, memory, a record or a table of values, even when it also says what the assistant checked.",
            "draft": "A draft message (an email, chat message, post or reply) that the assistant wrote for the reader to approve before it is sent to someone else.",
            "plain": "An ordinary conversational reply: an explanation, plan, proposal, design discussion or question.",
        }}),
    );
    let titles: Map<String, Value> =
        shown.iter().filter(|b| eligible(b, TITLE_CHARS)).map(|b| (b.i.to_string(), json!(plain(&b.text)))).collect();
    if titles.len() >= 2 {
        q.insert("title".into(), json!({"type": "choice", "instructions": TITLE_QUESTION, "criteria": titles}));
    }
    for b in shown {
        if b.kind != Kind::Heading && eligible(b, FACT_CHARS) {
            q.insert(
                format!("fact_{}", b.i),
                json!({"type": "noul", "instructions": FACT_QUESTION.replace("{n}", &b.i.to_string())}),
            );
        }
        if !urls(&b.text).is_empty() {
            q.insert(
                format!("link_{}", b.i),
                json!({"type": "noul", "instructions": LINK_QUESTION.replace("{n}", &b.i.to_string())}),
            );
        }
    }
    (state, q)
}

fn score(answers: &Value, key: &str) -> Option<f64> {
    answers[key]["noul"].as_f64()
}

/// The Result card for what Jev answered, or `None` when anything is short of its threshold. Every string is a block's
/// own text.
pub fn card_from(blocks: &[Block], answers: &Value) -> Option<Value> {
    let kind = &answers["kind"];
    if kind["choice"] != "info" || kind["probabilities"]["info"].as_f64()? < KIND_MIN {
        return None;
    }
    let pick = &answers["title"];
    let title_at: usize = pick["choice"].as_str()?.parse().ok()?;
    if pick["probabilities"][title_at.to_string()].as_f64()? < TITLE_MIN {
        return None;
    }
    let by_number = |n: usize| blocks.iter().find(|b| b.i == n && b.i <= MAX_BLOCKS);
    let title_block = by_number(title_at).filter(|b| eligible(b, TITLE_CHARS))?;
    // Highest scores first, then back in reply order.
    let best = |prefix: &str, cap: usize, allowed: &dyn Fn(&Block) -> bool| -> Vec<&Block> {
        let mut scored: Vec<(&Block, f64)> = blocks
            .iter()
            .filter(|b| b.i <= MAX_BLOCKS && allowed(b))
            .filter_map(|b| Some((b, score(answers, &format!("{prefix}_{}", b.i))?)))
            .filter(|(_, s)| *s >= SLOT_MIN)
            .collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1));
        scored.truncate(cap);
        scored.sort_by_key(|(b, _)| b.i);
        scored.into_iter().map(|(b, _)| b).collect()
    };
    // The lab's code rule: the title block is never also a fact.
    let facts =
        best("fact", MAX_FACTS, &|b| b.i != title_block.i && b.kind != Kind::Heading && eligible(b, FACT_CHARS));
    if facts.is_empty() {
        return None;
    }
    let links: Vec<Value> = best("link", MAX_LINKS, &|b| !urls(&b.text).is_empty())
        .into_iter()
        .filter_map(|b| {
            let url = urls(&b.text).into_iter().next()?;
            Some(json!({"label": link_label(&b.text, &url), "url": url}))
        })
        .collect();
    Some(json!({
        "type": "result",
        "v": 1,
        "snapshot": {
            "title": plain(&title_block.text),
            "facts": facts.iter().map(|b| plain(&b.text)).collect::<Vec<_>>(),
            "links": links,
        },
    }))
}

/// The card for `text`, or `None`: too plain, Jev unsure, or Jev failing (which is only logged).
pub async fn classify(jev: &Jev, text: &str) -> Option<Value> {
    let blocks = split(text);
    if !prefilter(text, &blocks) {
        return None;
    }
    let (state, questions) = ask_for(&blocks);
    match jev.ask(state, questions).await {
        Ok(v) => card_from(&blocks, &v["answers"]),
        Err(error) => {
            tracing::info!(error = format!("{error:#}"), "reply card: Jev didn't answer; the reply stays plain");
            None
        }
    }
}

// ---- gates ----

/// What the host knows about the run that wrote the reply.
#[derive(Clone, Copy, Debug)]
pub struct Run {
    /// The owner's own run: not an invited person's, a bot guest's, a routine's or another bot's ask.
    pub owner: bool,
    /// The chat is a 1:1, or a room read just now to hold only the owner's own bots.
    pub owner_only_room: bool,
    /// The turn touched the secrets deny list or raised a Key or Access card.
    pub touched_secrets: bool,
}

/// Why this reply may not be sent to Jev, or `None` when it may.
pub fn refusal(store: &Store, run: Run) -> Option<&'static str> {
    if !enabled(store) {
        Some("the owner hasn't turned reply cards on")
    } else if !run.owner {
        Some("not an owner run")
    } else if !run.owner_only_room {
        Some("a room with other people or outside bots")
    } else if run.touched_secrets {
        Some("the turn touched secrets")
    } else {
        None
    }
}

/// Whether a Key or Access card was raised in `chat` since `from` (ms): those are the turn's, whoever asked.
pub fn raised_secret_card(store: &Store, chat: &str, from: i64) -> bool {
    store.entries_from(chat, from).unwrap_or_default().iter().any(|e| {
        e.kind == crate::store::EntryKind::Permission.as_str()
            && matches!(e.data["card"].as_str(), Some("key" | "account"))
    })
}

/// The Jev key: the router's (environment or saved), else the one hq-auth grants this host.
async fn find(hub: &Arc<Hub>) -> Option<Jev> {
    if let Some(jev) = Jev::configured(hub).await {
        return Some(jev);
    }
    let paths = tokio::task::spawn_blocking(crate::channels::hq::Paths::discover).await.ok().flatten()?;
    let out =
        crate::channels::hq::output(&paths.auth_bin, &["get", "--for", "serve", "jev-browser", "TYPESAFE_API_KEY"])
            .await?;
    let key = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (out.status.success() && !key.is_empty()).then(|| Jev::from_key(key))
}

/// Attaches the Result card to `entry_id`'s reply when it earns one; never blocks the reply (call it from a task) and
/// never reports a failure to anyone. `jev` is the client to use, else the key is looked for.
pub async fn attach(hub: Arc<Hub>, entry_id: String, text: String, jev: Option<Jev>) {
    let work = async {
        let jev = match jev {
            Some(j) => j,
            None => find(&hub).await?,
        };
        classify(&jev, &text).await
    };
    let Some(card) = tokio::time::timeout(OVERALL, work).await.ok().flatten() else { return };
    // Read again now: the reply must still be this final reply, and nobody else has put a card on it.
    let Some(mut entry) = hub.store.entry(&entry_id) else { return };
    if entry.data["final"] != true
        || entry.data["text"].as_str() != Some(text.as_str())
        || !entry.data["richCard"].is_null()
    {
        return;
    }
    entry.data["richCard"] = card;
    hub.set_entry(&entry_id, &entry.data);
}

#[cfg(test)]
#[path = "jev_tests.rs"]
mod tests;
