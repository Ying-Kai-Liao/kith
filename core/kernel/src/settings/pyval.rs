//! Python's text for the values a settings file holds: `repr` and `str` of a JSON value (an error names the value it
//! refuses), `float()` of a typed amount and the truth of a value (`if not chosen`).

use serde_json::Value;
use std::fmt::Write as _;

/// `repr(s)` of a string: single quotes unless the text has a `'` and no `"`.
pub fn str_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::from(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                let _ = write!(out, "\\x{:02x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// `repr(v)` of a JSON value as Python loaded it.
pub fn repr(v: &Value) -> String {
    match v {
        Value::Null => "None".to_owned(),
        Value::Bool(b) => if *b { "True" } else { "False" }.to_owned(),
        Value::Number(_) => crate::secrets::pyjson::dumps(v),
        Value::String(s) => str_repr(s),
        Value::Array(a) => format!("[{}]", a.iter().map(repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter()
                .map(|(k, v)| format!("{}: {}", str_repr(k), repr(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// `str(v)`: a string as it is, anything else as its `repr`.
pub fn text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        v => repr(v),
    }
}

/// `bool(v)`.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `v == w` for the numbers Python holds equal whatever their type (`5 == 5.0`), else plain equality.
pub fn same(v: &Value, w: &Value) -> bool {
    match (v, w) {
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
        _ => v == w,
    }
}

/// `float(s)`: surrounding white space is allowed, a single `_` between digits too; `inf` and `nan` read as well.
pub fn float(s: &str) -> Option<f64> {
    let t = s.trim();
    let b = t.as_bytes();
    for (i, c) in b.iter().enumerate() {
        if *c == b'_' {
            let digit = |j: Option<&u8>| j.is_some_and(u8::is_ascii_digit);
            if !(i > 0 && digit(b.get(i - 1)) && digit(b.get(i + 1))) {
                return None;
            }
        }
    }
    t.replace('_', "").parse().ok()
}
