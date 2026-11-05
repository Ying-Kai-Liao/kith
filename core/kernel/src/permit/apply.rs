//! `kith-permit apply SETTINGS_FILE`: a guest run's launch asks the host for the access this session was given (a yes on an
//! escape card, an "always") and adds it to the settings file (`kith_guard.apply_grants`). Never fails the run: no answer is no
//! access, and a file that can't take the grants is left as it was.

use super::host::call_host;
use super::pyval::{os_error, repr_str, truthy, type_name};
use super::{Env, Out};
use crate::launch::guard::{
    IN_FOLDER_FILES, IN_FOLDER_TREES, both, dedup, edit_rules, rule_w, secret_paths, valid_domain,
};
use crate::paths::realpath;
use crate::secrets::pyjson;
use serde_json::{Map, Value};
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;
use std::time::Duration;

type R<T> = Result<T, String>;

const MAC: bool = cfg!(target_os = "macos");

fn fold(s: &str) -> String {
    if MAC { s.to_lowercase() } else { s.to_owned() }
}

/// Whether a deny entry (a path, `dir/**` for a tree, `dir/**/name` at any depth) holds `path`. Case-folded on macOS.
pub fn covers(entry: &str, path: &str) -> bool {
    let (entry, path) = (fold(entry), fold(path));
    let e = entry.strip_suffix("/**").unwrap_or(&entry);
    let parts: Vec<&str> = e.split("/**/").collect();
    reach(&parts, path.as_str())
}

/// `re.fullmatch(literal0 + "/(?:.*/)?" + literal1 ... + "(?:/.*)?", path)`: `.` takes no newline.
fn reach(parts: &[&str], path: &str) -> bool {
    let Some(rest) = path.strip_prefix(parts[0]) else {
        return false;
    };
    if parts.len() == 1 {
        return rest.is_empty() || rest.strip_prefix('/').is_some_and(|t| !t.contains('\n'));
    }
    let Some(after) = rest.strip_prefix('/') else {
        return false;
    };
    // the optional `.*/`: nothing, or any run without a newline that ends in a slash
    if reach(&parts[1..], after) {
        return true;
    }
    after
        .char_indices()
        .any(|(i, c)| c == '/' && !after[..i].contains('\n') && reach(&parts[1..], &after[i + 1..]))
}

/// Whether `path` is `inner` or a folder above it (a grant of `path` would reach `inner`).
fn holds(path: &str, inner: &str) -> bool {
    let (path, inner) = (fold(path), fold(inner));
    inner == path || inner.starts_with(&format!("{}/", path.trim_end_matches('/')))
}

/// Whether `path` is, or is below, a place rule W closes.
fn is_rule_w(home: &str, path: &str) -> bool {
    let (mut t, mut f) = rule_w(home, &[], &[]);
    t.extend(IN_FOLDER_TREES.iter().map(|n| format!("/**/{n}")));
    f.extend(IN_FOLDER_FILES.iter().map(|n| format!("/**/{n}")));
    t.iter().chain(&f).any(|x| covers(x, path))
}

fn subscript<'a>(v: &'a mut Value, key: &str) -> R<&'a mut Value> {
    match v {
        Value::Object(m) => m.get_mut(key).ok_or_else(|| repr_str(key)),
        Value::Array(_) => Err("list indices must be integers or slices, not str".to_owned()),
        Value::String(_) => Err("string indices must be integers, not 'str'".to_owned()),
        other => Err(format!(
            "'{}' object is not subscriptable",
            type_name(other)
        )),
    }
}

fn dict<'a>(v: &'a mut Value, what: &str) -> R<&'a mut Map<String, Value>> {
    let t = type_name(v);
    v.as_object_mut()
        .ok_or_else(|| format!("'{t}' object has no attribute '{what}'"))
}

/// `d.setdefault(key, [])` as a list.
fn list_of<'a>(d: &'a mut Map<String, Value>, key: &str) -> R<&'a mut Vec<Value>> {
    let v = d
        .entry(key.to_owned())
        .or_insert_with(|| Value::Array(vec![]));
    let t = type_name(v);
    v.as_array_mut()
        .ok_or_else(|| format!("'{t}' object has no attribute 'append'"))
}

fn put(list: &mut Vec<Value>, x: &str) {
    if !list.iter().any(|v| v.as_str() == Some(x)) {
        list.push(Value::String(x.to_owned()));
    }
}

fn strs(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// What `[x for x in grants.get(k, []) if isinstance(x, str)]` makes of the value the host sent: a list is filtered, a string
/// is its characters, an object its keys; anything else is not iterable.
fn grant_list(v: Option<&Value>) -> R<Vec<String>> {
    match v {
        None => Ok(vec![]),
        Some(Value::Array(a)) => Ok(a
            .iter()
            .filter_map(|x| x.as_str().map(str::to_owned))
            .collect()),
        Some(Value::String(s)) => Ok(s.chars().map(String::from).collect()),
        Some(Value::Object(o)) => Ok(o.keys().cloned().collect()),
        Some(other) => Err(format!("'{}' object is not iterable", type_name(other))),
    }
}

/// `real(p)`: an absolute path with no link or `..` on the way.
fn real(p: &str) -> Option<&str> {
    (p.starts_with('/') && realpath(Path::new(p)).to_string_lossy() == p).then_some(p)
}

fn perm_list<'a>(s: &'a mut Value, name: &str) -> R<&'a mut Vec<Value>> {
    let perms = dict(subscript(s, "permissions")?, "setdefault")?;
    list_of(perms, name)
}

fn fs_list<'a>(s: &'a mut Value, name: &str) -> R<&'a mut Vec<Value>> {
    let fs = subscript(subscript(s, "sandbox")?, "filesystem")?;
    list_of(dict(fs, "setdefault")?, name)
}

/// `kith_guard.apply_grants`: adds `grants` to the guest settings `s`, in place. `secrets` is the list of absolute secret
/// folders. Returns the (kind, value) pairs that went in; the rest are dropped.
fn apply_grants(
    s: &mut Value,
    grants: &[(&str, Vec<String>)],
    home: &str,
    secrets: &[String],
    env: &Env,
) -> R<Vec<(&'static str, String)>> {
    let grant = |k: &str| {
        grants
            .iter()
            .find(|(n, _)| *n == k)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    let fs = subscript(subscript(s, "sandbox")?, "filesystem")?;
    let fs = dict(fs, "get")?;
    // Named denies (other agents, other people, the owner's user.md) and the secrets stay on top of everything below. The
    // Kith folder and agents folder are denied whole with a way back in, so a grant inside them is judged by the named ones.
    let whole: Vec<String> = ["KITH_HOME", "KITH_AGENTS"]
        .iter()
        .filter_map(|r| env.get(r))
        .flat_map(|p| both(&absolute(p)))
        .collect();
    let never_read: Vec<String> = strs(fs.get("denyRead"))
        .into_iter()
        .filter(|d| d != "/" && !whole.contains(d))
        .chain(secrets.iter().cloned())
        .collect();
    let never_write: Vec<String> = strs(fs.get("denyWrite"))
        .into_iter()
        .chain(secrets.iter().cloned())
        .collect();
    // `permissions` and its three lists exist from here on, whether or not anything goes in
    let top = dict(s, "setdefault")?;
    let perms = top
        .entry("permissions".to_owned())
        .or_insert_with(|| Value::Object(Map::new()));
    let perms = dict(perms, "setdefault")?;
    for name in ["deny", "allow", "additionalDirectories"] {
        list_of(perms, name)?;
    }
    // no home to judge rule W against: add nothing (Python would ask the password database; the settings file's own
    // denyWrite still holds)
    if home.is_empty() {
        return Ok(vec![]);
    }
    let writes = grant("write");
    let mut done = vec![];
    for kind in ["read", "write"] {
        for p in dedup(grant(kind)) {
            let Some(p) = real(&p) else { continue };
            if never_read.iter().any(|d| covers(d, p)) || secrets.iter().any(|d| holds(p, d)) {
                continue;
            }
            let is_dir = Path::new(p).is_dir();
            let edit = if is_dir {
                format!("Edit(/{p}/**)")
            } else {
                format!("Edit(/{p})")
            };
            if kind == "write" {
                if never_write.iter().any(|d| covers(d, p)) || is_rule_w(home, p) {
                    continue;
                }
                let (trees, files) = rule_w(home, &[p.to_owned()], &[]);
                for t in &trees {
                    put(fs_list(s, "denyWrite")?, t);
                    if t.contains("**") {
                        put(fs_list(s, "denyWrite")?, &format!("{t}/**"));
                    }
                }
                for f in &files {
                    put(fs_list(s, "denyWrite")?, f);
                }
                for r in edit_rules(&trees, &files) {
                    put(perm_list(s, "deny")?, &r);
                }
                put(fs_list(s, "allowWrite")?, p);
                put(perm_list(s, "allow")?, &edit);
            } else if !writes.iter().any(|w| real(w).is_some() && w == p) {
                // read only: the file tools may not write there, whatever the permission mode would allow
                put(perm_list(s, "deny")?, &edit);
            }
            put(fs_list(s, "allowRead")?, p);
            if is_dir {
                put(perm_list(s, "additionalDirectories")?, p);
            } else {
                put(perm_list(s, "allow")?, &format!("Read(/{p})"));
            }
            done.push((if kind == "read" { "read" } else { "write" }, p.to_owned()));
        }
    }
    for d in dedup(grant("domains")) {
        if valid_domain(&d) {
            let sandbox = dict(subscript(s, "sandbox")?, "setdefault")?;
            let net = sandbox
                .entry("network".to_owned())
                .or_insert_with(|| Value::Object(Map::new()));
            put(list_of(dict(net, "setdefault")?, "allowedDomains")?, &d);
            done.push(("domain", d));
        }
    }
    Ok(done)
}

/// `os.path.abspath`: against the working folder, `.` and `..` folded away.
fn absolute(p: &str) -> String {
    let joined = if p.starts_with('/') {
        p.to_owned()
    } else {
        format!(
            "{}/{p}",
            std::env::current_dir()
                .unwrap_or_default()
                .to_string_lossy()
        )
    };
    let mut parts: Vec<&str> = vec![];
    for c in joined.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            c => parts.push(c),
        }
    }
    format!("/{}", parts.join("/"))
}

/// Adds the access this session was given to a guest run's settings file.
pub fn apply(env: &Env, kernel: &Path, settings_file: &str) -> Out {
    let reply = call_host(env, "consumeAccess", &Map::new(), Duration::from_secs(10));
    let grants = reply
        .as_ref()
        .filter(|r| r.get("ok") == Some(&Value::Bool(true)))
        .and_then(|r| r.get("grants"))
        .and_then(Value::as_object);
    let Some(grants) = grants else {
        return Out::default();
    };
    if !["read", "write", "domains"]
        .iter()
        .any(|k| grants.get(*k).is_some_and(truthy))
    {
        return Out::default();
    }
    match add(env, kernel, settings_file, grants) {
        Ok(given) => Out {
            stderr: given
                .iter()
                .map(|(kind, v)| format!("kith: access given: {kind} {v}\n"))
                .collect(),
            ..Out::default()
        },
        Err(e) => Out {
            stderr: format!(
                "kith: could not add the granted access ({e}); the run goes on without it\n"
            ),
            ..Out::default()
        },
    }
}

fn add(
    env: &Env,
    kernel: &Path,
    settings_file: &str,
    grants: &Map<String, Value>,
) -> R<Vec<(&'static str, String)>> {
    let p = env.paths(kernel);
    let text = std::fs::read(settings_file).map_err(|e| os_error(&e, settings_file))?;
    let text = String::from_utf8_lossy(&text);
    let mut s: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => return Err(pyjson::error_of(&text).unwrap_or_else(|| e.to_string())),
    };
    let list = |k: &str| grant_list(grants.get(k));
    let wanted = [
        ("read", list("read")?),
        ("write", list("write")?),
        ("domains", list("domains")?),
    ];
    let (state, machine, secrets_dir) = (
        p.state.to_string_lossy().into_owned(),
        p.machine.to_string_lossy().into_owned(),
        p.secrets.to_string_lossy().into_owned(),
    );
    // the secrets folder counts whole: no guest needs any of it
    let mut secrets = secret_paths(&[state, machine.clone()], &secrets_dir, &machine);
    secrets.extend(both(&secrets_dir));
    let secrets = dedup(secrets);
    let home = env.home.to_string_lossy().into_owned();
    let done = apply_grants(&mut s, &wanted, &home, &secrets, env)?;
    let dir = Path::new(settings_file)
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let (tmp, mut f) = temp_file(dir).map_err(|e| os_error(&e, &dir.to_string_lossy()))?;
    f.write_all(pyjson::dumps_indent_n(&s, 1).as_bytes())
        .map_err(|e| os_error(&e, &tmp.to_string_lossy()))?;
    drop(f);
    std::fs::rename(&tmp, settings_file).map_err(|e| os_error(&e, &tmp.to_string_lossy()))?;
    Ok(done)
}

/// `tempfile.mkstemp(dir=...)`: a new file only the user can read.
fn temp_file(dir: &Path) -> std::io::Result<(std::path::PathBuf, std::fs::File)> {
    let mut n = std::process::id();
    loop {
        let name = dir.join(format!(
            "tmp{:08x}",
            n.wrapping_mul(2_654_435_761)
                ^ std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.subsec_nanos())
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&name)
        {
            Ok(f) => return Ok((name, f)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => n = n.wrapping_add(1),
            Err(e) => return Err(e),
        }
    }
}
