//! The pieces of Python's standard library and `kith_trajectory.redact` that the must-ask judge (`judge.rs`) leans on,
//! ported so the answers stay the same: `shlex` (posix, punctuation chars), `fnmatch`, `str.split()` and `redact`.

use std::sync::LazyLock;

use regex::{NoExpand, Regex};

/// Python's `str.isspace` for one char (Rust's `is_whitespace` lacks the four information separators).
pub fn is_py_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// `str.split()`: runs of whitespace separate, none stay.
pub fn py_split(s: &str) -> Vec<String> {
    s.split(is_py_space)
        .filter(|w| !w.is_empty())
        .map(str::to_owned)
        .collect()
}

/// `str.strip()`.
pub fn py_strip(s: &str) -> &str {
    s.trim_matches(is_py_space)
}

const PUNCT: &str = ";&|()<>";

#[derive(Clone, Copy, PartialEq)]
enum St {
    Space,
    Word,
    Punct,
    Quote(char),
    Escape,
    End,
}

struct Lex {
    chars: Vec<char>,
    pos: usize,
    push: Vec<char>,
    state: St,
}

impl Lex {
    /// `shlex.shlex.read_token` with `posix=True`, `whitespace_split=True`, `punctuation_chars=";&|()<>"`: `Ok(None)` at
    /// the end, `Err` for the `ValueError` of an open quote or a trailing backslash.
    fn read_token(&mut self) -> Result<Option<String>, ()> {
        let (mut token, mut quoted, mut escaped) = (String::new(), false, ' ');
        loop {
            let next = if let Some(c) = self.push.pop() {
                Some(c)
            } else {
                let c = self.chars.get(self.pos).copied();
                self.pos += usize::from(c.is_some());
                c
            };
            match self.state {
                St::End => break,
                St::Space => {
                    let Some(c) = next else {
                        self.state = St::End;
                        break;
                    };
                    if " \t\r\n".contains(c) {
                        if !token.is_empty() || quoted {
                            break;
                        }
                    } else if c == '#' {
                        self.readline();
                    } else if c == '\\' {
                        escaped = 'a';
                        self.state = St::Escape;
                    } else if PUNCT.contains(c) {
                        token = c.to_string();
                        self.state = St::Punct;
                    } else if c == '\'' || c == '"' {
                        self.state = St::Quote(c);
                    } else {
                        token = c.to_string();
                        self.state = St::Word;
                    }
                }
                St::Quote(q) => {
                    quoted = true;
                    let Some(c) = next else { return Err(()) };
                    if c == q {
                        self.state = St::Word;
                    } else if c == '\\' && q == '"' {
                        escaped = q;
                        self.state = St::Escape;
                    } else {
                        token.push(c);
                    }
                }
                St::Escape => {
                    let Some(c) = next else { return Err(()) };
                    if (escaped == '\'' || escaped == '"') && c != '\\' && c != escaped {
                        token.push('\\');
                    }
                    token.push(c);
                    self.state = if escaped == 'a' {
                        St::Word
                    } else {
                        St::Quote(escaped)
                    };
                }
                St::Word | St::Punct => {
                    let Some(c) = next else {
                        self.state = St::End;
                        break;
                    };
                    if " \t\r\n".contains(c) {
                        self.state = St::Space;
                        if !token.is_empty() || quoted {
                            break;
                        }
                    } else if c == '#' {
                        self.readline();
                        self.state = St::Space;
                        if !token.is_empty() || quoted {
                            break;
                        }
                    } else if self.state == St::Punct {
                        if PUNCT.contains(c) {
                            token.push(c);
                        } else {
                            self.push.push(c);
                            self.state = St::Space;
                            break;
                        }
                    } else if c == '\'' || c == '"' {
                        self.state = St::Quote(c);
                    } else if c == '\\' {
                        escaped = 'a';
                        self.state = St::Escape;
                    } else if !PUNCT.contains(c) {
                        token.push(c);
                    } else {
                        self.push.push(c);
                        self.state = St::Space;
                        if !token.is_empty() || quoted {
                            break;
                        }
                    }
                }
            }
        }
        Ok((quoted || !token.is_empty()).then_some(token))
    }

    /// `instream.readline()`: what is left of the line is dropped.
    fn readline(&mut self) {
        while let Some(c) = self.chars.get(self.pos) {
            self.pos += 1;
            if *c == '\n' {
                break;
            }
        }
    }
}

/// `list(shlex.shlex(s, posix=True, punctuation_chars=";&|()<>"))` with `whitespace_split`, `None` on its `ValueError`.
pub fn shlex_words(s: &str) -> Option<Vec<String>> {
    let mut lex = Lex {
        chars: s.chars().collect(),
        pos: 0,
        push: vec![],
        state: St::Space,
    };
    let mut out = vec![];
    while let Some(t) = lex.read_token().ok()? {
        out.push(t);
    }
    Some(out)
}

/// `fnmatch.fnmatch(name, pat)` on a POSIX system (no case folding): `*`, `?` and `[seq]` / `[!seq]`, `*` crossing slashes.
pub fn fnmatch(name: &str, pat: &str) -> bool {
    let (n, p): (Vec<char>, Vec<char>) = (name.chars().collect(), pat.chars().collect());
    glob(&n, &p)
}

fn glob(n: &[char], p: &[char]) -> bool {
    let Some((&c, rest)) = p.split_first() else {
        return n.is_empty();
    };
    match c {
        '*' => (0..=n.len()).any(|i| glob(&n[i..], rest)),
        '?' => !n.is_empty() && glob(&n[1..], rest),
        '[' => {
            let mut j = 1;
            if p.get(j) == Some(&'!') {
                j += 1;
            }
            if p.get(j) == Some(&']') {
                j += 1;
            }
            while j < p.len() && p[j] != ']' {
                j += 1;
            }
            if j >= p.len() {
                return n.first() == Some(&'[') && glob(&n[1..], rest);
            }
            let (neg, set) = match p[1..j].split_first() {
                Some((&'!', s)) => (true, s),
                _ => (false, &p[1..j]),
            };
            let Some(&ch) = n.first() else { return false };
            let mut hit = false;
            let mut k = 0;
            while k < set.len() {
                if k + 2 < set.len() && set[k + 1] == '-' {
                    hit |= set[k] <= ch && ch <= set[k + 2];
                    k += 3;
                } else {
                    hit |= set[k] == ch;
                    k += 1;
                }
            }
            hit != neg && glob(&n[1..], &p[j + 1..])
        }
        c => n.first() == Some(&c) && glob(&n[1..], rest),
    }
}

// ---- redact (kith_trajectory.py): the same replacements in the same order, so `redact(t) != t` agrees.

const NAME: &str = r"[\w.-]*(?:secret|token|passw(?:or)?d|passphrase|api[_-]?key|private[_-]?key|access[_-]?key|credential|authorization|_key|_pass|_dsn|database_url|db_url|dsn)[\w.-]*";

/// (pattern, replacement, whether the replacement names groups).
type Rule = (Regex, &'static str, bool);

#[allow(clippy::unwrap_used)] // fixed patterns
static REDACTIONS: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    let name_rule =
        format!(r#"(?i)(["']?{NAME}["']?\s*[:=]\s*)(?:"[^"\n]*"|'[^'\n]*'|[^\s,;}}\]]+)"#);
    let rows: Vec<(String, &'static str, bool)> = vec![
        (
            r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?(?:-----END [A-Z ]*PRIVATE KEY-----|\z)"
                .into(),
            "[redacted]",
            false,
        ),
        (
            r"(?s)-----BEGIN [A-Z ]*(?:CERTIFICATE|KEY)[A-Z ]*-----.*?(?:-----END [A-Z ]*-----|\z)"
                .into(),
            "[redacted]",
            false,
        ),
        (
            r"(?i)\b([a-z][a-z0-9+.-]*://[^\s:/@]+:)[^\s@/]+@".into(),
            "${1}[redacted]@",
            true,
        ),
        (
            r"(?i)\b(bearer|basic)\s+[A-Za-z0-9._~+/=-]{8,}".into(),
            "${1} [redacted]",
            true,
        ),
        (
            r"\b(?:[sr]k_(?:live|test)_[A-Za-z0-9]{8,}|glpat-[A-Za-z0-9_-]{12,}|npm_[A-Za-z0-9]{12,}|hf_[A-Za-z0-9]{12,}|ya29\.[A-Za-z0-9_-]{12,}|sk-[A-Za-z0-9_-]{12,}|gh[pousr]_[A-Za-z0-9]{12,}|github_pat_[A-Za-z0-9_]{12,}|xox[abprs]-[A-Za-z0-9-]{8,}|(?:AKIA|ASIA)[0-9A-Z]{12,}|AIza[0-9A-Za-z_-]{20,}|eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{4,})".into(),
            "[redacted]",
            false,
        ),
        (name_rule, "${1}[redacted]", true),
    ];
    rows.into_iter()
        .map(|(p, r, e)| (Regex::new(&p).unwrap(), r, e))
        .collect()
});

/// Replaces every match of `re` (a pattern with no lookarounds) that `ok(char before, char after)` accepts with
/// `[redacted]`: the lookbehind and lookahead of the Python pattern, by hand. A rejected match is retried one char on.
fn sub_around(text: &str, re: &Regex, ok: impl Fn(Option<char>, Option<char>) -> bool) -> String {
    let (mut out, mut last, mut at) = (String::new(), 0, 0);
    while at <= text.len() {
        let Some(m) = re.find_at(text, at) else { break };
        let (before, after) = (
            text[..m.start()].chars().next_back(),
            text[m.end()..].chars().next(),
        );
        if ok(before, after) {
            out.push_str(&text[last..m.start()]);
            out.push_str("[redacted]");
            last = m.end();
            at = m.end().max(m.start() + 1);
        } else {
            at = m.start() + text[m.start()..].chars().next().map_or(1, char::len_utf8);
        }
    }
    out.push_str(&text[last..]);
    out
}

fn luhn(digits: &str) -> bool {
    let mut total = 0;
    for (i, ch) in digits.chars().rev().enumerate() {
        let mut d = ch.to_digit(10).unwrap_or(0);
        if i % 2 == 1 {
            d = if d > 4 { d * 2 - 9 } else { d * 2 };
        }
        total += d;
    }
    total % 10 == 0
}

/// `re.sub(r'(?<!\d)\d(?:[ -]?\d){12,18}(?!\d)', ...)`: a run of 13 to 19 digits with single spaces or dashes between,
/// redacted when it passes the Luhn check. Only ASCII digits count (Python's `\d` also takes other scripts' digits).
fn cards(text: &str) -> String {
    let c: Vec<(usize, char)> = text.char_indices().collect();
    let digit = |i: usize| c.get(i).is_some_and(|(_, ch)| ch.is_ascii_digit());
    let (mut out, mut i, mut last) = (String::new(), 0, 0);
    while i < c.len() {
        if !digit(i) || (i > 0 && digit(i - 1)) {
            i += 1;
            continue;
        }
        // ends[k]: the index after k+1 digits, taken the way the greedy group takes them
        let (mut ends, mut j) = (vec![i + 1], i + 1);
        while ends.len() < 19 {
            if digit(j) {
                j += 1;
            } else if matches!(c.get(j), Some((_, ' ' | '-'))) && digit(j + 1) {
                j += 2;
            } else {
                break;
            }
            ends.push(j);
        }
        let hit = (13..=ends.len())
            .rev()
            .map(|n| ends[n - 1])
            .find(|e| !digit(*e));
        let Some(e) = hit else {
            i += 1;
            continue;
        };
        let (from, to) = (c[i].0, c.get(e).map_or(text.len(), |x| x.0));
        let digits: String = text[from..to]
            .chars()
            .filter(char::is_ascii_digit)
            .collect();
        if luhn(&digits) {
            out.push_str(&text[last..from]);
            out.push_str("[redacted]");
            last = to;
        }
        i = e;
    }
    out.push_str(&text[last..]);
    out
}

/// `kith_trajectory.redact`.
#[allow(clippy::unwrap_used)] // fixed patterns
pub fn redact(text: &str) -> String {
    static TW: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[A-Z][1289]\d{8}").unwrap());
    static SSN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d{3}-\d{2}-\d{4}").unwrap());
    static ND: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d$").unwrap());
    let mut t = text.to_owned();
    for (re, repl, expand) in REDACTIONS.iter() {
        t = if *expand {
            re.replace_all(&t, *repl).into_owned()
        } else {
            re.replace_all(&t, NoExpand(repl)).into_owned()
        };
    }
    let alnum = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric());
    let digit = |c: Option<char>| c.is_some_and(|c| ND.is_match(c.encode_utf8(&mut [0; 4])));
    t = sub_around(&t, &TW, |b, a| !alnum(b) && !alnum(a));
    t = sub_around(&t, &SSN, |b, a| !digit(b) && !digit(a));
    cards(&t)
}
