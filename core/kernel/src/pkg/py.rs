//! The bits of Python that `kernel/bin/kith-pkg` leans on, so the port fails with the same words: an exception that is
//! printed (`str(e)`), `Path.read_text` (universal newlines, strict UTF-8), `str.strip` (Unicode whitespace) and `repr`
//! of a string.

use std::fs;
use std::io;
use std::path::Path;

use serde_json::Value;

use crate::secrets::pyjson;

/// Which exception class a failure stands for: `resolve` catches some of them (`except (RuntimeError, ValueError, OSError,
/// TimeoutExpired)` around the fetch) and lets the rest through to the per-agent fallback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Os,
    /// `ValueError`, which `UnicodeDecodeError` is a kind of.
    Value,
    Runtime,
    Timeout,
    /// Any other exception (`AttributeError`, `TypeError`, ...).
    Other,
    /// `SystemExit` with this code: `die()`. Nothing catches it but the process.
    Exit(i32),
}

/// A raised exception, as `str(e)` prints it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PyErr {
    pub kind: Kind,
    pub msg: String,
}

pub type R<T> = Result<T, PyErr>;

impl std::fmt::Display for PyErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.msg)
    }
}

impl PyErr {
    pub fn new(kind: Kind, msg: impl Into<String>) -> Self {
        Self {
            kind,
            msg: msg.into(),
        }
    }

    /// `die(msg, code)`: the text for stderr (with its prefix) and the exit code.
    pub fn die(msg: &str, code: i32) -> Self {
        Self::new(Kind::Exit(code), format!("kith-pkg: {msg}"))
    }

    /// Whether `isinstance(e, OSError)`.
    pub fn is_os(&self) -> bool {
        self.kind == Kind::Os
    }
}

/// An `OSError` from `e` on `path`: `[Errno 2] No such file or directory: '/x'`.
pub fn os_err(e: &io::Error, path: &Path) -> PyErr {
    let text = e.to_string();
    let reason = text
        .rsplit_once(" (os error ")
        .map_or(text.as_str(), |(r, _)| r);
    let msg = match e.raw_os_error() {
        Some(n) => format!(
            "[Errno {n}] {reason}: {}",
            repr(&path.to_string_lossy())
        ),
        None => reason.to_owned(),
    };
    PyErr::new(Kind::Os, msg)
}

/// `str.isspace()` for one character (what `\s` and `strip()` mean for a `str`).
pub fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

/// `s.strip()`.
pub fn strip(s: &str) -> &str {
    s.trim_matches(is_space)
}

/// `repr(s)` for a `str`.
pub fn repr(s: &str) -> String {
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
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c if (0x80..0xa0).contains(&(c as u32)) => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// The text of the `UnicodeDecodeError` Python raises for these bytes.
fn decode_error(bytes: &[u8], e: &std::str::Utf8Error) -> String {
    let start = e.valid_up_to();
    let (span, reason) = match e.error_len() {
        None => (1, "unexpected end of data"),
        Some(n) => {
            let lead = bytes[start];
            let why = if (0x80..=0xc1).contains(&lead) || lead >= 0xf5 {
                "invalid start byte"
            } else {
                "invalid continuation byte"
            };
            (n, why)
        }
    };
    let which = if span == 1 {
        format!("byte 0x{:02x} in position {start}", bytes[start])
    } else {
        format!("bytes in position {start}-{}", start + span - 1)
    };
    format!("'utf-8' codec can't decode {which}: {reason}")
}

/// `Path.read_text()`: UTF-8, `\r\n` and `\r` read as `\n`.
pub fn read_text(path: &Path) -> R<String> {
    let bytes = fs::read(path).map_err(|e| os_err(&e, path))?;
    match std::str::from_utf8(&bytes) {
        Ok(s) => Ok(if s.contains('\r') {
            s.replace("\r\n", "\n").replace('\r', "\n")
        } else {
            s.to_owned()
        }),
        Err(e) => Err(PyErr::new(Kind::Value, decode_error(&bytes, &e))),
    }
}

/// `Path.write_text(text)`.
pub fn write_text(path: &Path, text: &str) -> R<()> {
    fs::write(path, text).map_err(|e| os_err(&e, path))
}

/// `json.loads(text)`: the value, or the `ValueError` text. (`NaN` and `Infinity`, which Python reads and serde does not, come
/// back as `null`: nothing here looks at a number.)
pub fn loads(text: &str) -> Result<Value, String> {
    if let Some(e) = pyjson::error_of(text) {
        return Err(e);
    }
    Ok(serde_json::from_str(text).unwrap_or(Value::Null))
}

/// `type(v).__name__`.
pub fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

/// `AttributeError: 'list' object has no attribute 'get'`.
pub fn no_attr(v: &Value, attr: &str) -> PyErr {
    PyErr::new(
        Kind::Other,
        format!("'{}' object has no attribute '{attr}'", type_name(v)),
    )
}

/// `Path.is_dir()` (follows links; any error is false).
pub fn is_dir(p: &Path) -> bool {
    fs::metadata(p).is_ok_and(|m| m.is_dir())
}

/// `Path.is_file()`.
pub fn is_file(p: &Path) -> bool {
    fs::metadata(p).is_ok_and(|m| m.is_file())
}

/// `Path.exists()`.
pub fn exists(p: &Path) -> bool {
    fs::metadata(p).is_ok()
}

/// `time.time()`.
pub fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// `st_mtime` as a float.
pub fn mtime(m: &fs::Metadata) -> f64 {
    use std::os::unix::fs::MetadataExt;
    m.mtime() as f64 + m.mtime_nsec() as f64 / 1e9
}

/// A character of Python's `\w` (a letter, a digit or `_`).
pub fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}
