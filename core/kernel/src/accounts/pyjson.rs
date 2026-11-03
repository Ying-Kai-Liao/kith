//! JSON text the way Python's `json.dumps` writes it (`ensure_ascii`, `", "` and `": "` separators, or `indent=2`), so a
//! file this module writes is byte for byte what `kernel/lib/kith_accounts.py` and `kith_settings.py` write, and what the
//! difftest compares.

use serde_json::Value;
use std::fmt::Write as _;

/// `json.dumps(v)`.
pub fn dumps(v: &Value) -> String {
    let mut out = String::new();
    write(v, &mut out, false, 0);
    out
}

/// `json.dumps(v, indent=2)`.
pub fn dumps_pretty(v: &Value) -> String {
    let mut out = String::new();
    write(v, &mut out, true, 0);
    out
}

/// A JSON string literal, non-ASCII as `\uXXXX` (surrogate pairs above the BMP).
pub fn quote(s: &str, out: &mut String) {
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

/// Python's `repr(float)`: the shortest text that reads back, an exponent (two digits at least, signed) from 1e16 up and
/// below 1e-4.
pub fn float_repr(f: f64) -> String {
    if f.is_nan() {
        return "NaN".to_owned();
    }
    if f.is_infinite() {
        return if f > 0.0 { "Infinity" } else { "-Infinity" }.to_owned();
    }
    let a = f.abs();
    if a != 0.0 && !(1e-4..1e16).contains(&a) {
        let e = format!("{f:e}"); // 1.5e-7, 1e16
        let (mantissa, exp) = e.split_once('e').unwrap_or((&e, "0"));
        let n: i32 = exp.parse().unwrap_or(0);
        return format!("{mantissa}e{}{:02}", if n < 0 { '-' } else { '+' }, n.abs());
    }
    let s = f.to_string();
    if s.contains('.') { s } else { format!("{s}.0") }
}

fn newline(out: &mut String, level: usize) {
    out.push('\n');
    for _ in 0..level {
        out.push_str("  ");
    }
}

fn write(v: &Value, out: &mut String, pretty: bool, level: usize) {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Number(n) => match (n.as_i64(), n.as_u64()) {
            (Some(i), _) => {
                let _ = write!(out, "{i}");
            }
            (None, Some(u)) => {
                let _ = write!(out, "{u}");
            }
            _ => out.push_str(&float_repr(n.as_f64().unwrap_or(0.0))),
        },
        Value::String(s) => quote(s, out),
        Value::Array(items) => {
            if items.is_empty() {
                out.push_str("[]");
                return;
            }
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                    if !pretty {
                        out.push(' ');
                    }
                }
                if pretty {
                    newline(out, level + 1);
                }
                write(item, out, pretty, level + 1);
            }
            if pretty {
                newline(out, level);
            }
            out.push(']');
        }
        Value::Object(fields) => {
            if fields.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push('{');
            for (i, (k, item)) in fields.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                    if !pretty {
                        out.push(' ');
                    }
                }
                if pretty {
                    newline(out, level + 1);
                }
                quote(k, out);
                out.push_str(": ");
                write(item, out, pretty, level + 1);
            }
            if pretty {
                newline(out, level);
            }
            out.push('}');
        }
    }
}
