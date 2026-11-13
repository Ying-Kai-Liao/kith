//! The regular expressions of `kernel/bin/kith-pkg`, written out by hand (the crate has no regex engine, and each of these
//! is a few lines): the frontmatter of `agent.md`, `needs:` in `capabilities.yaml`, `{{PLACEHOLDER}}`s, and the skills an
//! agent's text names.

use super::py::{is_space, is_word, strip};

/// `re.match(r'^---\n(.*?)\n---\n?', text, re.S)`: `(the frontmatter, the rest)`.
fn split(text: &str) -> Option<(&str, &str)> {
    let rest = text.strip_prefix("---\n")?;
    let j = rest.find("\n---")?;
    let after = &rest[j + 4..];
    Some((&rest[..j], after.strip_prefix('\n').unwrap_or(after)))
}

/// One try of `^([A-Za-z_-]+):\s*(.*)$` (multi-line) at the start of a line: `(key, value, end)`. `\s*` runs over newlines, so
/// `description:` with nothing after it takes the next line as its value.
fn field_at(s: &str, pos: usize) -> Option<(&str, &str, usize)> {
    let key_len = s[pos..]
        .bytes()
        .take_while(|b| b.is_ascii_alphabetic() || *b == b'_' || *b == b'-')
        .count();
    let colon = pos + key_len;
    if key_len == 0 || s.as_bytes().get(colon) != Some(&b':') {
        return None;
    }
    let after = &s[colon + 1..];
    let start = colon + 1 + (after.len() - after.trim_start_matches(is_space).len());
    let end = s[start..].find('\n').map_or(s.len(), |n| start + n);
    Some((&s[pos..colon], &s[start..end], end))
}

/// `dict(re.findall(...))` over the frontmatter, in order (a later key replaces an earlier one on lookup).
fn fields_of(group: &str) -> Vec<(String, String)> {
    let mut out = vec![];
    let mut pos = 0;
    while pos < group.len() {
        let line_start = pos == 0 || group.as_bytes()[pos - 1] == b'\n';
        if line_start && let Some((k, v, end)) = field_at(group, pos) {
            out.push((k.to_owned(), v.to_owned()));
            pos = end.max(pos + 1);
            continue;
        }
        pos += group[pos..].chars().next().map_or(1, char::len_utf8);
    }
    out
}

/// The frontmatter fields of an `agent.md` and the text after it.
pub struct Front {
    fields: Vec<(String, String)>,
    pub body: String,
}

impl Front {
    /// `fields.get(key)`.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields
            .iter()
            .rev()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// `frontmatter(text)`.
pub fn frontmatter(text: &str) -> Front {
    match split(text) {
        Some((group, body)) => Front {
            fields: fields_of(group),
            body: body.to_owned(),
        },
        None => Front {
            fields: vec![],
            body: text.to_owned(),
        },
    }
}

/// The start of every line of `text` (what `^` means under `re.M`).
fn lines(text: &str) -> impl Iterator<Item = &str> {
    let mut at = Some(0);
    std::iter::from_fn(move || {
        let start = at?;
        at = text[start..].find('\n').map(|n| start + n + 1);
        Some(&text[start..])
    })
}

/// `re.search(r'^needs:\s*\[([^\]]*)\]', text, re.M)`: what is between the brackets.
pub fn needs_list(text: &str) -> Option<&str> {
    lines(text).find_map(|line| {
        let rest = line.strip_prefix("needs:")?;
        let rest = rest.trim_start_matches(is_space).strip_prefix('[')?;
        let close = rest.find(']')?;
        Some(&rest[..close])
    })
}

/// `re.search(r'^needs:', text, re.M)`.
pub fn has_needs(text: &str) -> bool {
    lines(text).any(|l| l.starts_with("needs:"))
}

/// `[s.strip() for s in group.split(',') if s.strip()]`.
pub fn split_needs(group: &str) -> Vec<String> {
    group
        .split(',')
        .map(strip)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

/// `set(re.findall(r'\{\{([A-Z_]+)\}\}', text))`, in no order.
pub fn placeholders(text: &str) -> Vec<String> {
    let b = text.as_bytes();
    let mut out: Vec<String> = vec![];
    let mut i = 0;
    while i + 1 < b.len() {
        if b[i] == b'{' && b[i + 1] == b'{' {
            let name = b[i + 2..]
                .iter()
                .take_while(|c| c.is_ascii_uppercase() || **c == b'_')
                .count();
            let end = i + 2 + name;
            if name > 0 && b.get(end) == Some(&b'}') && b.get(end + 1) == Some(&b'}') {
                let n = text[i + 2..end].to_owned();
                if !out.contains(&n) {
                    out.push(n);
                }
                i = end + 2;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// `re.findall(r'`([\w./-]+/SKILL\.md)`', body)`.
pub fn skill_refs(body: &str) -> Vec<String> {
    const TAIL: &str = "/SKILL.md";
    let mut out = vec![];
    let mut i = 0;
    while let Some(off) = body[i..].find('`') {
        let start = i + off + 1;
        let run = body[start..]
            .find(|c: char| !(is_word(c) || c == '.' || c == '/' || c == '-'))
            .unwrap_or(body.len() - start);
        let end = start + run;
        if body[end..].starts_with('`') && body[start..end].ends_with(TAIL) && run > TAIL.len() {
            out.push(body[start..end].to_owned());
            i = end + 1;
        } else {
            i = start;
        }
    }
    out
}
