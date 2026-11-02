//! Python's `json` module, as far as auth.json needs it: `json.dumps` (compact and `indent=2`, ASCII only) and the text
//! of the error `json.loads` raises, so a damaged file is reported the way `kith-auth` reports it.

use serde_json::Value;
use std::fmt::Write as _;

/// `json.dumps(v)`: `", "` and `": "` separators.
pub fn dumps(v: &Value) -> String {
    let mut out = String::new();
    write(v, None, 0, &mut out);
    out
}

/// `json.dumps(v, indent=2)`.
pub fn dumps_indent(v: &Value) -> String {
    let mut out = String::new();
    write(v, Some(2), 0, &mut out);
    out
}

fn write(v: &Value, indent: Option<usize>, level: usize, out: &mut String) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => out.push_str(&number(n)),
        Value::String(s) => quote(s, out),
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, x) in items.iter().enumerate() {
                separator(i, indent, level + 1, out);
                write(x, indent, level + 1, out);
            }
            close(indent, level, out);
            out.push(']');
        }
        Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (i, (k, x)) in map.iter().enumerate() {
                separator(i, indent, level + 1, out);
                quote(k, out);
                out.push_str(": ");
                write(x, indent, level + 1, out);
            }
            close(indent, level, out);
            out.push('}');
        }
    }
}

fn separator(i: usize, indent: Option<usize>, level: usize, out: &mut String) {
    match indent {
        Some(n) => {
            if i > 0 {
                out.push(',');
            }
            out.push('\n');
            out.push_str(&" ".repeat(n * level));
        }
        None if i > 0 => out.push_str(", "),
        None => {}
    }
}

fn close(indent: Option<usize>, level: usize, out: &mut String) {
    if let Some(n) = indent {
        out.push('\n');
        out.push_str(&" ".repeat(n * level));
    }
}

/// A number as `repr` prints it: a float in plain digits from 1e-4 up to 1e16, else with an exponent that carries its
/// sign and at least two digits (`1e+16`, `1e-05`).
fn number(n: &serde_json::Number) -> String {
    let Some(f) = n.as_f64().filter(|_| n.is_f64()) else {
        return n.to_string();
    };
    // `{:e}` gives the shortest digits that read back the same, as `repr` does: `1.5e-5`
    let sci = format!("{f:e}");
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    if !(-4..16).contains(&exp) {
        let sign = if exp < 0 { '-' } else { '+' };
        return format!("{mantissa}e{sign}{:02}", exp.abs());
    }
    let (negative, mantissa) = match mantissa.strip_prefix('-') {
        Some(m) => (true, m),
        None => (false, mantissa),
    };
    let digits: String = mantissa.chars().filter(char::is_ascii_digit).collect();
    // `exp + 1` digits sit before the point
    let body = match usize::try_from(exp + 1) {
        Ok(p) if p >= digits.len() && p > 0 => {
            format!("{digits}{}.0", "0".repeat(p - digits.len()))
        }
        Ok(p) if p > 0 => format!("{}.{}", &digits[..p], &digits[p..]),
        _ => format!(
            "0.{}{digits}",
            "0".repeat(usize::try_from(-(exp + 1)).unwrap_or(0))
        ),
    };
    format!("{}{body}", if negative { "-" } else { "" })
}

fn quote(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (' '..='~').contains(&c) => out.push(c),
            c => {
                let mut units = [0u16; 2];
                for u in c.encode_utf16(&mut units) {
                    let _ = write!(out, "\\u{u:04x}");
                }
            }
        }
    }
    out.push('"');
}

// --- the error json.loads would raise ----------------------------------------------------------------------------------

/// What `json.loads(text)` says about a text that is not JSON: `Expecting value: line 1 column 1 (char 0)`. `None` when
/// Python reads it (including the `NaN` and `Infinity` it accepts and serde does not).
pub fn error_of(text: &str) -> Option<String> {
    let s: Vec<char> = text.chars().collect();
    let Err((msg, pos)) = top(&s) else {
        return None;
    };
    let line = s[..pos].iter().filter(|c| **c == '\n').count() + 1;
    let col = match s[..pos].iter().rposition(|c| *c == '\n') {
        Some(i) => pos - i,
        None => pos + 1,
    };
    Some(format!("{msg}: line {line} column {col} (char {pos})"))
}

type E = (&'static str, usize);

fn ws(s: &[char], mut i: usize) -> usize {
    while matches!(s.get(i), Some(' ' | '\t' | '\n' | '\r')) {
        i += 1;
    }
    i
}

fn top(s: &[char]) -> Result<(), E> {
    let end = value(s, ws(s, 0))?;
    let end = ws(s, end);
    if end == s.len() {
        Ok(())
    } else {
        Err(("Extra data", end))
    }
}

fn word(s: &[char], i: usize, w: &str) -> bool {
    w.chars().enumerate().all(|(k, c)| s.get(i + k) == Some(&c))
}

fn digits(s: &[char], mut i: usize) -> usize {
    while s.get(i).is_some_and(char::is_ascii_digit) {
        i += 1;
    }
    i
}

fn value(s: &[char], i: usize) -> Result<usize, E> {
    match s.get(i) {
        Some('"') => string(s, i + 1),
        Some('{') => object(s, i + 1),
        Some('[') => array(s, i + 1),
        Some('n') if word(s, i, "null") => Ok(i + 4),
        Some('t') if word(s, i, "true") => Ok(i + 4),
        Some('f') if word(s, i, "false") => Ok(i + 5),
        Some('N') if word(s, i, "NaN") => Ok(i + 3),
        Some('I') if word(s, i, "Infinity") => Ok(i + 8),
        Some('-') if word(s, i, "-Infinity") => Ok(i + 9),
        Some('-' | '0'..='9') => {
            let mut j = i + usize::from(s[i] == '-');
            match s.get(j) {
                Some('0') => j += 1,
                Some('1'..='9') => j = digits(s, j),
                _ => return Err(("Expecting value", i)),
            }
            if s.get(j) == Some(&'.') && digits(s, j + 1) > j + 1 {
                j = digits(s, j + 1);
            }
            if matches!(s.get(j), Some('e' | 'E')) {
                let k = j + 1 + usize::from(matches!(s.get(j + 1), Some('+' | '-')));
                if digits(s, k) > k {
                    j = digits(s, k);
                }
            }
            Ok(j)
        }
        _ => Err(("Expecting value", i)),
    }
}

/// `begin`: the index after the opening quote.
fn string(s: &[char], begin: usize) -> Result<usize, E> {
    let mut i = begin;
    loop {
        match s.get(i) {
            None => return Err(("Unterminated string starting at", begin - 1)),
            Some('"') => return Ok(i + 1),
            Some(c) if (*c as u32) < 0x20 => return Err(("Invalid control character at", i)),
            Some('\\') => match s.get(i + 1) {
                None => return Err(("Unterminated string starting at", begin - 1)),
                Some('u') => {
                    if s.len() < i + 6 || !s[i + 2..i + 6].iter().all(char::is_ascii_hexdigit) {
                        return Err(("Invalid \\uXXXX escape", i + 1));
                    }
                    i += 6;
                }
                Some('"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't') => i += 2,
                Some(_) => return Err(("Invalid \\escape", i)),
            },
            Some(_) => i += 1,
        }
    }
}

fn object(s: &[char], i: usize) -> Result<usize, E> {
    let mut i = ws(s, i);
    if s.get(i) == Some(&'}') {
        return Ok(i + 1);
    }
    loop {
        if s.get(i) != Some(&'"') {
            return Err(("Expecting property name enclosed in double quotes", i));
        }
        i = ws(s, string(s, i + 1)?);
        if s.get(i) != Some(&':') {
            return Err(("Expecting ':' delimiter", i));
        }
        i = ws(s, value(s, ws(s, i + 1))?);
        match s.get(i) {
            Some('}') => return Ok(i + 1),
            Some(',') => i = ws(s, i + 1),
            _ => return Err(("Expecting ',' delimiter", i)),
        }
    }
}

fn array(s: &[char], i: usize) -> Result<usize, E> {
    let mut i = ws(s, i);
    if s.get(i) == Some(&']') {
        return Ok(i + 1);
    }
    loop {
        i = ws(s, value(s, i)?);
        match s.get(i) {
            Some(']') => return Ok(i + 1),
            Some(',') => i = ws(s, i + 1),
            _ => return Err(("Expecting ',' delimiter", i)),
        }
    }
}
