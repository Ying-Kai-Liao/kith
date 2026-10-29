//! The News card: a scheduled run's brief, as `kith-send` sent it, in rows (title, one line, source · domain, link).
//! Code finds the rows in the text; Telegram and an app that doesn't know the card keep the text as sent.
use serde_json::{Value, json};

#[derive(Debug, PartialEq, Eq)]
pub struct Row {
    pub title: String,
    pub line: String,
    pub source: Option<String>,
    pub domain: String,
    pub url: String,
}

const SEPARATORS: [&str; 4] = [" — ", " – ", " - ", ": "];
const TRIM: [char; 9] = [' ', '—', '–', '-', ':', '·', '|', ',', ';'];

/// The News card for a scheduled run's text, or `None` when it isn't a brief: at least two list items, each with a
/// web link, and around them nothing but a lead before and a tail after.
pub fn card(text: &str, agent: &str, job: &str, cron: Option<&str>) -> Option<Value> {
    let (lead, items, tail) = split(text)?;
    let rows = items.iter().map(|i| row(i)).collect::<Option<Vec<_>>>()?;
    if rows.len() < 2 {
        return None;
    }
    let rows: Vec<Value> = rows
        .into_iter()
        .map(|r| json!({"title": r.title, "line": r.line, "source": r.source, "domain": r.domain, "url": r.url}))
        .collect();
    Some(json!({
        "type": "news",
        "v": 1,
        "ref": {"agent": agent, "job": job},
        "snapshot": {"name": super::job_name(job), "every": cron.map(super::cron_words), "lead": lead, "tail": tail, "rows": rows},
    }))
}

/// A list item's text: `- `, `* `, `• `, `– `, `1. ` or `1) `.
fn bullet(line: &str) -> Option<&str> {
    let line = line.trim_start();
    if let Some(rest) = ["- ", "* ", "• ", "– "].iter().find_map(|b| line.strip_prefix(b)) {
        return Some(rest.trim());
    }
    let digits = line.find(|c: char| !c.is_ascii_digit())?;
    if !(1..=2).contains(&digits) {
        return None;
    }
    line[digits..].strip_prefix(". ").or_else(|| line[digits..].strip_prefix(") ")).map(str::trim)
}

/// The lead, the list items (each with the lines that continue it) and the tail; `None` when other text sits
/// between items.
fn split(text: &str) -> Option<(String, Vec<String>, String)> {
    let (mut lead, mut items, mut tail) = (Vec::new(), Vec::<String>::new(), Vec::new());
    let mut open = false;
    for line in text.lines() {
        if let Some(item) = bullet(line) {
            if tail.iter().any(|l: &&str| !l.trim().is_empty()) {
                return None;
            }
            tail.clear();
            items.push(item.to_owned());
            open = true;
        } else if line.trim().is_empty() {
            open = false;
            if items.is_empty() { lead.push(line) } else { tail.push(line) }
        } else if let Some(last) = items.last_mut().filter(|_| tail.iter().all(|l| l.trim().is_empty()))
            && (open || line.starts_with([' ', '\t']))
        {
            last.push(' ');
            last.push_str(line.trim());
        } else if items.is_empty() {
            lead.push(line);
        } else {
            tail.push(line);
        }
    }
    Some((lead.join("\n").trim().to_owned(), items, tail.join("\n").trim().to_owned()))
}

/// The item's link: its URL, the label of a markdown link, and where the link sits in the item.
fn link(item: &str) -> Option<(String, Option<String>, usize, usize)> {
    if let Some(mid) = item.find("](http") {
        let open = item[..mid].rfind('[')?;
        let close = mid + 2 + item[mid + 2..].find(')')?;
        return Some((item[mid + 2..close].to_owned(), Some(item[open + 1..mid].to_owned()), open, close + 1));
    }
    let start = item.find("https://").or_else(|| item.find("http://"))?;
    let len = item[start..].find(char::is_whitespace).unwrap_or(item.len() - start);
    let url = item[start..start + len].trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '"', '\'']);
    Some((url.to_owned(), None, start, start + url.len()))
}

fn domain(url: &str) -> Option<String> {
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
    let host = rest.split(['/', '?', '#']).next()?.rsplit('@').next()?.split(':').next()?.to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host).to_owned();
    (host.contains('.') && !host.contains(char::is_whitespace)).then_some(host)
}

fn clean(text: &str) -> String {
    let plain = text.replace("**", "").replace("__", "").replace('`', "");
    let words = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    words
        .trim_start_matches(TRIM)
        .trim_start_matches(')')
        .trim_end_matches(TRIM)
        .trim_end_matches('(')
        .trim()
        .to_owned()
}

/// A short label set apart from the sentence: the source's name ("GitHub", "BOM").
fn source(label: &str) -> Option<String> {
    let label = clean(label);
    (!label.is_empty() && label.chars().count() <= 40 && !label.contains("://")).then_some(label)
}

/// One item as a row: the title (bold, a leading link, or the words before a dash or colon), the rest as its line,
/// and a link label set apart at the end as its source.
fn row(item: &str) -> Option<Row> {
    let (url, label, start, end) = link(item)?;
    let domain = domain(&url)?;
    let (mut before, after) = (item[..start].trim_end().to_owned(), item[end..].trim_start());
    let rest_empty = after.trim_matches(|c: char| c.is_whitespace() || ".)".contains(c)).is_empty();
    let mut found = None;
    let text = match label {
        Some(label) if start == 0 => {
            let title = clean(&label);
            return (!title.is_empty()).then(|| Row { title, line: clean(after), source: None, domain, url });
        }
        Some(label) if rest_empty => {
            found = source(&label);
            before
        }
        Some(label) => format!("{before} {label} {after}"),
        None => {
            let mut after = after;
            // "… (BOM, https://…)": the words in the brackets name the source.
            if let Some(rest) = after.strip_prefix(')')
                && let Some(paren) = before.rfind('(')
            {
                found = source(&before[paren + 1..]);
                before.truncate(paren);
                after = rest;
            }
            // The full stop after a link that ends the item.
            if after.trim_matches(|c: char| c.is_whitespace() || c == '.').is_empty() {
                after = "";
            }
            format!("{before} {after}")
        }
    };
    let text = text.trim();
    let (title, line) = if let Some(bold) = text.strip_prefix("**")
        && let Some(close) = bold.find("**")
    {
        (&bold[..close], &bold[close + 2..])
    } else if let Some((at, sep)) =
        SEPARATORS.iter().filter_map(|s| text.find(s).map(|at| (at, s.len()))).min_by_key(|(at, _)| *at)
    {
        (&text[..at], &text[at + sep..])
    } else if let Some(at) = text.find(". ") {
        (&text[..=at], &text[at + 2..])
    } else {
        (text, "")
    };
    let title = clean(title);
    (!title.is_empty()).then(|| Row { title, line: clean(line), source: found, domain, url })
}
