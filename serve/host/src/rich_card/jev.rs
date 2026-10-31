//! Jev reply cards (DESIGN.md "Reply cards", step 8): behind an owner opt-in, a finished reply that reads like a result
//! (a lookup, a status report, a table of figures) gets a Result card attached to it. The reply stays: `data.text` is the
//! card's twin, and the apps fold the card back to it ("Show as text").
//!
//! Jev only picks. Code splits the reply into numbered blocks; in ONE call Jev says whether the reply is a result,
//! which block is its title, which is its one-line verdict (or none), and, for every block that splits into a label and
//! a value (a table row, a `Key: value` line or item), how much the reader needs it and whether it is only a log line,
//! a suggestion or a remark about the assistant. Every string on the card is copied from those blocks (markdown marks
//! stripped), so Jev never writes a word. Exclusion vetoes, importance ranks. Anything short of a clear answer leaves
//! the reply plain, and Jev being missing or down is never shown to anyone. Asking cards (Key, Access, permission)
//! never come from here.
//!
//! Method from the lab's `jev-reply-cards` and `jev-result-card-check` experiments (kind `info` at 0.85, links at
//! 0.5); the v2 picks are justified in the PR from live runs.

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
/// A title, verdict line, row label or row value longer than this reads as prose, not a card line. The lab found
/// 80/100 hid 4 of 13 real cards; 160 doubled recall with no loss in precision.
const TITLE_CHARS: usize = 160;
const HEADLINE_CHARS: usize = 200;
const LABEL_CHARS: usize = 40;
const VALUE_CHARS: usize = 160;
/// Lab thresholds: the reply is a result at this probability, a block fills a slot at this score.
const KIND_MIN: f64 = 0.85;
const TITLE_MIN: f64 = 0.6;
const LINK_MIN: f64 = 0.5;
/// The verdict line must be picked at this probability, "none" included in the choices: below it `sub` is left out.
const HEADLINE_MIN: f64 = 0.6;
/// A row needs this much "the reader needs it" to be kept (ranked, then cut to [`MAX_ROWS`]) ...
const IMPORTANT_MIN: f64 = 0.6;
/// ... and is dropped when its "this is a log line, suggestion or remark about the assistant" reaches this.
const EXCLUDED_MIN: f64 = 0.7;
const MIN_ROWS: usize = 1;
const MAX_ROWS: usize = 5;
/// A paragraph among the first blocks may offer its first sentence as the title when the whole is too long.
const OPENING_BLOCKS: usize = 3;
const MAX_LINKS: usize = 2;
/// The title and headline questions' "no block does" option.
const NONE: &str = "none";
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
    /// The block as a card row: a table row's first cell and the next non-empty one, or the two sides of a
    /// `Label: value` line or item; marks removed. `None` when it doesn't split that way.
    pub pair: Option<(String, String)>,
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

/// `Label: value` in a line without marks: a short label of a few words, then a colon and a space.
fn label_value(line: &str) -> Option<(String, String)> {
    let colon = line
        .char_indices()
        .take(LABEL_CHARS + 1)
        .find(|(i, c)| *c == ':' && line[i + 1..].starts_with(char::is_whitespace))
        .map(|(i, _)| i)?;
    let label = line[..colon].trim();
    let value = line[colon + 1..].trim().trim_end_matches('.').trim();
    let plausible = !label.is_empty()
        && label.split_whitespace().count() <= 5
        && label.chars().all(|c| c.is_alphanumeric() || " _/&'-()".contains(c));
    (plausible && !value.is_empty()).then(|| (label.to_owned(), value.to_owned()))
}

/// A bare figure: a number with at most a short unit or percent sign after it ("31.1", "about 5.3", "12%").
fn figure(cell: &str) -> bool {
    let c = cell.trim().trim_start_matches(['~', '≈']).trim();
    let c = c.strip_prefix("about ").unwrap_or(c);
    let digits = c.chars().take_while(|ch| ch.is_ascii_digit() || ".,".contains(*ch)).count();
    digits > 0 && c[digits..].trim().chars().count() <= 4
}

/// The unit a table header names for its column ("GB", "Size (GB)"), when it is one of the usual ones.
fn column_unit(header: &str) -> Option<&str> {
    const UNITS: [&str; 15] =
        ["B", "KB", "MB", "GB", "TB", "KiB", "MiB", "GiB", "TiB", "ms", "s", "%", "MHz", "GHz", "kB"];
    let word = |c: char| !c.is_alphanumeric() && c != '%';
    header.split(word).find(|t| UNITS.contains(t))
}

/// A table row as label and value. The label is the first cell, unless that one is a bare figure and another cell is
/// words (`| GB | What |` tables): then the words label the figure. A figure under a unit header gets the unit.
fn table_pair(cells: &[&str], header: Option<&[String]>) -> Option<(String, String)> {
    let cells: Vec<String> = cells.iter().map(|c| plain(c)).collect();
    let words = |c: &&String| !c.is_empty() && !figure(c);
    let label_at = if figure(&cells[0]) { cells.iter().position(|c| words(&c)).unwrap_or(0) } else { 0 };
    let value_at = if label_at == 0 { (1..cells.len()).find(|i| !cells[*i].is_empty())? } else { 0 };
    if cells[label_at].is_empty() {
        return None;
    }
    let mut value = cells[value_at].clone();
    let unit = header.and_then(|h| h.get(value_at)).and_then(|h| column_unit(h));
    if let Some(unit) = unit.filter(|u| figure(&value) && !value.contains(*u)) {
        value = if unit == "%" { format!("{value}%") } else { format!("{value} {unit}") };
    }
    Some((cells[label_at].clone(), value))
}

fn add_with(blocks: &mut Vec<Block>, kind: Kind, text: &str, pair: Option<(String, String)>) {
    let text = text.trim();
    if !text.is_empty() {
        blocks.push(Block { i: blocks.len() + 1, kind, text: text.to_owned(), pair });
    }
}

fn add(blocks: &mut Vec<Block>, kind: Kind, text: &str) {
    let pair = matches!(kind, Kind::Item | Kind::Kv).then(|| label_value(&plain(text))).flatten();
    add_with(blocks, kind, text, pair);
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
    // The previous line was a table row: a separator right after it makes that row the table's header.
    let mut after_row = false;
    // The header cells of the table being read (marks removed): the unit of a value column is often only named there.
    let mut header: Option<Vec<String>> = None;
    let mut last_cells: Option<Vec<String>> = None;
    let text = text.replace("\r\n", "\n");
    for line in text.split('\n') {
        let header_candidate = std::mem::take(&mut after_row);
        if !line.trim_start().starts_with('|') {
            header = None;
        }
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
            if table_separator(line) {
                if header_candidate && blocks.last().is_some_and(|b| b.kind == Kind::Row) {
                    blocks.pop();
                    header = last_cells.take();
                }
            } else {
                let cells: Vec<&str> = line.trim().trim_matches('|').split('|').map(str::trim).collect();
                let pair = table_pair(&cells, header.as_deref());
                let before = blocks.len();
                add_with(&mut blocks, Kind::Row, &cells.join(" | "), pair);
                last_cells = Some(cells.iter().map(|c| plain(c)).collect());
                after_row = blocks.len() > before;
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
const TITLE_QUESTION: &str = "Which block of `reply` best names what the reply is about, as a title for it?";
const HEADLINE_QUESTION: &str = "Which block of `reply` is its verdict or summary: the one sentence a reader who reads nothing else should get, such as how things stand overall? Answer none when no block does that.";
const IMPORTANT_QUESTION: &str = "Does the reader need block {n} of `reply` to know the state of its subject or to decide what to do, such as a figure that changed or is near a limit?";
const EXCLUDED_QUESTION: &str = "Is block {n} of `reply` a log of what the assistant did or checked, a suggestion or to-do for the reader, or a remark about the assistant, rather than a fact about the subject?";
const LINK_QUESTION: &str = "Is block {n} of `reply` a link that the reader would open to see the result itself?";

/// A block that can name the reply or sum it up: prose or a heading, not code and not a label/value row.
fn lead(b: &Block, max: usize) -> bool {
    b.kind != Kind::Code && b.pair.is_none() && b.i <= MAX_BLOCKS && plain(&b.text).chars().count() <= max
}

/// The first sentence of `text`: up to the first `.`, `!` or `?` that is followed by whitespace or the end (so
/// "22.7 GB" and "e.g." don't end it).
fn first_sentence(text: &str) -> &str {
    const ABBREVIATIONS: [&str; 7] = ["e.g.", "i.e.", "vs.", "etc.", "approx.", "no.", "st."];
    for (at, c) in text.char_indices() {
        if !matches!(c, '.' | '!' | '?') || !text[at + 1..].chars().next().is_none_or(char::is_whitespace) {
            continue;
        }
        let word = text[..=at].rsplit(char::is_whitespace).next().unwrap_or("").to_lowercase();
        if !ABBREVIATIONS.contains(&word.as_str()) {
            return &text[..=at];
        }
    }
    text
}

/// What block `b` offers as the reply's title: its text when short enough, else (for one of the first paragraphs) its
/// first sentence when that is. Always the reply's own words.
fn title_of(b: &Block) -> Option<String> {
    if lead(b, TITLE_CHARS) {
        return Some(plain(&b.text));
    }
    if b.kind != Kind::Para || b.pair.is_some() || b.i > OPENING_BLOCKS {
        return None;
    }
    let text = plain(&b.text);
    let first = first_sentence(&text);
    (first.chars().count() <= TITLE_CHARS).then(|| first.to_owned())
}

/// A block that can be a card row: it splits into a label and a value, both short enough to be a card line.
fn row(b: &Block) -> Option<&(String, String)> {
    b.pair
        .as_ref()
        .filter(|(l, v)| b.i <= MAX_BLOCKS && l.chars().count() <= LABEL_CHARS && v.chars().count() <= VALUE_CHARS)
}

/// What Jev is asked, all in one call: what the reply is (the lab's four-way wording, so a digest or a draft isn't
/// forced into "result"), its title, its verdict line, and per row candidate how much it matters and whether it is a
/// log line or a suggestion.
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
    let mut titles: Map<String, Value> =
        shown.iter().filter_map(|b| Some((b.i.to_string(), json!(title_of(b)?)))).collect();
    if !titles.is_empty() {
        // A reply whose only lead is a long sentence is better left plain than titled by it.
        titles.insert(NONE.into(), json!("None of the blocks works as a title for the reply."));
        q.insert("title".into(), json!({"type": "choice", "instructions": TITLE_QUESTION, "criteria": titles}));
    }
    let mut headlines: Map<String, Value> =
        shown.iter().filter(|b| lead(b, HEADLINE_CHARS)).map(|b| (b.i.to_string(), json!(plain(&b.text)))).collect();
    headlines.insert(NONE.into(), json!("None of the blocks is a verdict or summary of the whole reply."));
    q.insert("headline".into(), json!({"type": "choice", "instructions": HEADLINE_QUESTION, "criteria": headlines}));
    for b in shown {
        if row(b).is_some() {
            let n = b.i.to_string();
            q.insert(
                format!("need_{n}"),
                json!({"type": "noul", "instructions": IMPORTANT_QUESTION.replace("{n}", &n)}),
            );
            q.insert(
                format!("skip_{n}"),
                json!({"type": "noul", "instructions": EXCLUDED_QUESTION.replace("{n}", &n)}),
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

/// A choice's block number when it was picked at `min` or better.
fn picked(answer: &Value, min: f64) -> Option<usize> {
    let n: usize = answer["choice"].as_str()?.parse().ok()?;
    (answer["probabilities"][n.to_string()].as_f64()? >= min).then_some(n)
}

/// The blocks scoring at least `min`, the best `cap` of them, back in reply order.
fn best(scored: Vec<(&Block, f64)>, min: f64, cap: usize) -> Vec<&Block> {
    let mut scored: Vec<_> = scored.into_iter().filter(|(_, s)| *s >= min).collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    scored.truncate(cap);
    scored.sort_by_key(|(b, _)| b.i);
    scored.into_iter().map(|(b, _)| b).collect()
}

/// The Result card (v2) for what Jev answered, or `None` when anything is short of its threshold. Every string is a
/// block's own text.
pub fn card_from(blocks: &[Block], answers: &Value) -> Option<Value> {
    let kind = &answers["kind"];
    if kind["choice"] != "info" || kind["probabilities"]["info"].as_f64()? < KIND_MIN {
        return None;
    }
    let by_number = |n: usize| blocks.iter().find(|b| b.i == n && b.i <= MAX_BLOCKS);
    let title_block = by_number(picked(&answers["title"], TITLE_MIN)?)?;
    let title = title_of(title_block)?;
    // A verdict that Jev isn't sure of, or that repeats the title, is left out; the card still stands.
    let headline_block = picked(&answers["headline"], HEADLINE_MIN)
        .and_then(by_number)
        .filter(|b| b.i != title_block.i && lead(b, HEADLINE_CHARS) && plain(&b.text) != title);
    // Exclusion vetoes (a missing answer counts against the row), importance ranks.
    let rows = best(
        blocks
            .iter()
            .filter(|b| row(b).is_some() && b.i != title_block.i && headline_block.is_none_or(|h| h.i != b.i))
            .filter(|b| score(answers, &format!("skip_{}", b.i)).is_some_and(|s| s < EXCLUDED_MIN))
            .filter_map(|b| Some((b, score(answers, &format!("need_{}", b.i))?)))
            .collect(),
        IMPORTANT_MIN,
        MAX_ROWS,
    );
    if rows.len() < MIN_ROWS {
        return None;
    }
    let links: Vec<Value> = best(
        blocks
            .iter()
            .filter(|b| b.i <= MAX_BLOCKS && !urls(&b.text).is_empty())
            .filter_map(|b| Some((b, score(answers, &format!("link_{}", b.i))?)))
            .collect(),
        LINK_MIN,
        MAX_LINKS,
    )
    .into_iter()
    .filter_map(|b| {
        let url = urls(&b.text).into_iter().next()?;
        Some(json!({"label": link_label(&b.text, &url), "url": url}))
    })
    .collect();
    let mut snapshot = json!({
        "title": title,
        "rows": rows.iter().filter_map(|b| row(b)).map(|(l, v)| json!({"label": l, "value": v})).collect::<Vec<_>>(),
        "links": links,
    });
    if let Some(h) = headline_block {
        snapshot["sub"] = json!(plain(&h.text));
    }
    Some(json!({"type": "result", "v": 2, "snapshot": snapshot}))
}

/// What one reply made of Jev: the card, if any, and the answers behind it (`Null` when Jev wasn't asked).
pub struct Traced {
    pub card: Option<Value>,
    pub answers: Value,
}

/// [`classify`], keeping Jev's answers: for the eval harness and the logs.
pub async fn classify_traced(jev: &Jev, text: &str) -> anyhow::Result<Traced> {
    let blocks = split(text);
    if !prefilter(text, &blocks) {
        return Ok(Traced { card: None, answers: Value::Null });
    }
    let (state, questions) = ask_for(&blocks);
    let v = jev.ask(state, questions).await?;
    Ok(Traced { card: card_from(&blocks, &v["answers"]), answers: v["answers"].clone() })
}

/// The card for `text`, or `None`: too plain, Jev unsure, or Jev failing (which is only logged).
pub async fn classify(jev: &Jev, text: &str) -> Option<Value> {
    match classify_traced(jev, text).await {
        Ok(t) => {
            // Scores and choices only, never the reply's text.
            tracing::debug!(answers = %t.answers, carded = t.card.is_some(), "reply card: Jev answered");
            t.card
        }
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
