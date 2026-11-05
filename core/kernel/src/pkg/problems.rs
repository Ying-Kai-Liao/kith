//! The checks a working copy must pass to become the running version (`problems()` in `kernel/bin/kith-pkg`), and the parts of
//! the package they read: `capabilities.yaml`'s `needs:` and `skills.json`'s linked skills.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serde_json::Value;

use super::git::run;
use super::py::{
    Kind, PyErr, R, exists, is_dir, is_file, is_word, loads, no_attr, read_text, repr, type_name,
};
use super::text::{frontmatter, has_needs, needs_list, placeholders, skill_refs, split_needs};
use super::{Ctx, life, name_of};

const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
const PLACEHOLDERS: [&str; 11] = [
    "KITH",
    "HQ",
    "AGENTS",
    "PACKAGE",
    "AGENT",
    "AGENT_DIR",
    "HOME_DIR",
    "HOME_REAL",
    "STATE",
    "MACHINE",
    "USER_HOME",
];

/// Python's truth value of a JSON value.
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

/// `needs(home)`: the shared services this agent asks for (`needs: [a, b]` in `capabilities.yaml`).
pub fn needs(home: &Path) -> R<Vec<String>> {
    let f = home.join("capabilities.yaml");
    if !exists(&f) {
        return Ok(vec![]);
    }
    let text = read_text(&f)?;
    Ok(needs_list(&text).map(split_needs).unwrap_or_default())
}

/// `read(p)`: the text of a small file, stripped; empty when it cannot be read.
pub fn read(p: &Path) -> R<String> {
    match read_text(p) {
        Ok(t) => Ok(super::py::strip(&t).to_owned()),
        Err(e) if e.is_os() => Ok(String::new()),
        Err(e) => Err(e),
    }
}

/// `source_line(src)`: where a linked skill comes from, as `.source` records it.
pub fn source_line(src: &Value) -> String {
    let get = |k: &str| src.get(k).and_then(Value::as_str).unwrap_or("");
    let path = get("path").trim_matches('/');
    format!(
        "{}@{}:{}",
        get("repo"),
        get("ref"),
        if path.is_empty() { "." } else { path }
    )
}

fn is_name(s: &str) -> bool {
    let mut cs = s.chars();
    cs.next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && cs.all(|c| is_word(c) || c == '.' || c == '-')
}

fn is_repo(s: &str) -> bool {
    let part = |p: &str| !p.is_empty() && p.chars().all(|c| is_word(c) || c == '.' || c == '-');
    s.split_once('/').is_some_and(|(a, b)| part(a) && part(b))
}

fn is_sha(s: &str) -> bool {
    s.len() == 40
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn value_err(msg: String) -> PyErr {
    PyErr::new(Kind::Value, msg)
}

/// `re.fullmatch(pattern, src.get(key, ''))`: the value must be a string.
fn string_of<'a>(src: &'a Value, key: &str) -> R<&'a str> {
    match src.get(key) {
        None => Ok(""),
        Some(Value::String(s)) => Ok(s),
        Some(v) => Err(PyErr::new(
            Kind::Other,
            format!(
                "expected string or bytes-like object, got '{}'",
                type_name(v)
            ),
        )),
    }
}

/// `external_skills(home)`: `{folder name: {repo, path, ref, license}}` from `skills.json`: third-party skills linked, not
/// copied. A `ValueError` for a file that is not valid.
pub fn external_skills(home: &Path) -> R<Vec<(String, Value)>> {
    let f = home.join("skills.json");
    if !exists(&f) {
        return Ok(vec![]);
    }
    let invalid = |m: &str| value_err(format!("skills.json: invalid JSON ({m})"));
    let text = match read_text(&f) {
        Ok(t) => t,
        Err(e) if e.kind == Kind::Value => return Err(invalid(&e.msg)),
        Err(e) => return Err(e),
    };
    let data = loads(&text).map_err(|m| invalid(&m))?;
    let Value::Object(top) = &data else {
        return Err(no_attr(&data, "get"));
    };
    let skills = top
        .get("skills")
        .cloned()
        .unwrap_or_else(|| Value::Object(serde_json::Map::new()));
    let Value::Object(skills) = skills else {
        return Err(no_attr(&skills, "items"));
    };
    for (name, src) in &skills {
        if !is_name(name) {
            return Err(value_err(format!(
                "skills.json: bad skill folder name {}",
                repr(name)
            )));
        }
        if !src.is_object() {
            return Err(no_attr(src, "get"));
        }
        if !is_repo(string_of(src, "repo")?) {
            return Err(value_err(format!(
                "skills.json: {name}: repo must be owner/name"
            )));
        }
        if !is_sha(string_of(src, "ref")?) {
            return Err(value_err(format!(
                "skills.json: {name}: ref must be a full commit sha (pinned, never a branch)"
            )));
        }
        let path = src.get("path").unwrap_or(&Value::Null);
        let bad_path = || {
            value_err(format!(
                "skills.json: {name}: path must be a folder in the repo (\".\" for the repo itself)"
            ))
        };
        if !truthy(path) {
            return Err(bad_path());
        }
        let Value::String(p) = path else {
            return Err(no_attr(path, "split"));
        };
        if p.split('/').any(|s| s == "..") {
            return Err(bad_path());
        }
    }
    Ok(skills.into_iter().collect())
}

/// `problems(home)`: reasons a working copy must not become the running version. Empty list = good.
pub fn problems(ctx: &Ctx, home: &Path) -> R<Vec<String>> {
    let mut out: Vec<String> = vec![];
    let text = read_text(&home.join("agent.md"))?;
    let front = frontmatter(&text);
    if front.get("description").is_none_or(str::is_empty) {
        out.push("agent.md: frontmatter has no description".to_owned());
    }
    if let Some(e) = front.get("effort").filter(|e| !e.is_empty())
        && !EFFORTS.contains(&e)
    {
        out.push(format!(
            "agent.md: effort {} is not one of {}",
            repr(e),
            EFFORTS.join(", ")
        ));
    }
    if let Some(m) = front.get("model").filter(|m| !m.is_empty())
        && !m.chars().all(|c| is_word(c) || ".[]-".contains(c))
    {
        out.push(format!(
            "agent.md: model {} is not a model name or alias",
            repr(m)
        ));
    }
    for name in ["settings.json", "mcp.json"] {
        let f = home.join(name);
        if !exists(&f) {
            continue;
        }
        let bad = match read_text(&f) {
            Ok(t) => loads(&t).err(),
            Err(e) if e.kind == Kind::Value => Some(e.msg),
            Err(e) => return Err(e),
        };
        if let Some(m) = bad {
            out.push(format!("{name}: invalid JSON ({m})"));
        }
    }
    for name in ["agent.md", "settings.json"] {
        let f = home.join(name);
        let mut unknown: Vec<String> = if exists(&f) {
            placeholders(&read_text(&f)?)
        } else {
            vec![]
        };
        unknown.retain(|p| !PLACEHOLDERS.contains(&p.as_str()));
        unknown.sort();
        if !unknown.is_empty() {
            out.push(format!(
                "{name}: unknown placeholder(s) {}",
                unknown.join(", ")
            ));
        }
    }
    let caps = home.join("capabilities.yaml");
    if exists(&caps) {
        let t = read_text(&caps)?;
        if has_needs(&t) && needs_list(&t).is_none() {
            out.push("capabilities.yaml: write needs as a one-line list, needs: [a, b]".to_owned());
        }
    }
    let linked = match external_skills(home) {
        Ok(l) => l,
        Err(e) if e.kind == Kind::Value => {
            out.push(e.msg);
            vec![]
        }
        Err(e) => return Err(e),
    };
    for (name, src) in &linked {
        if read(&home.join("skills").join(name).join(".source"))? != source_line(src) {
            let reference = src.get("ref").and_then(Value::as_str).unwrap_or("");
            out.push(format!(
                "skills/{name} is not fetched at {} (kith-pkg fetch {})",
                reference.chars().take(12).collect::<String>(),
                name_of(home)
            ));
        }
    }
    for skill in skill_refs(&front.body) {
        let skill = skill.strip_prefix("skills/").unwrap_or(&skill);
        if !is_file(&home.join("skills").join(skill)) {
            out.push(format!(
                "agent.md names skills/{skill}, which does not exist"
            ));
        }
    }
    for svc in needs(home)? {
        let file = format!("{svc}.json");
        if !is_file(&ctx.kernel.join("services").join(&file))
            && !is_file(&home.join("services").join(&file))
        {
            out.push(format!(
                "capabilities.yaml needs service {}, not in kernel/services/ or its own services/",
                repr(&svc)
            ));
        }
    }
    out.extend(service_problems(home)?);
    out.extend(must_ask_problems(ctx, home)?);
    Ok(out)
}

/// The `services/*.json` a package brings: each must say what kind of service it is.
fn service_problems(home: &Path) -> R<Vec<String>> {
    let dir = home.join("services");
    let mut files: Vec<_> = match fs::read_dir(&dir) {
        Ok(rd) if is_dir(&dir) => rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect(),
        _ => vec![],
    };
    files.sort();
    let mut out = vec![];
    for f in files {
        let name = name_of(&f);
        let parsed = match read_text(&f) {
            Ok(t) => loads(&t),
            Err(e) if e.kind == Kind::Value => Err(e.msg),
            Err(e) => return Err(e),
        };
        match parsed {
            Ok(v) => {
                let Value::Object(o) = &v else {
                    return Err(no_attr(&v, "get"));
                };
                if !o.get("kind").is_some_and(truthy) {
                    out.push(format!("services/{name}: no kind (mcp, cli or login)"));
                }
            }
            Err(m) => out.push(format!("services/{name}: invalid JSON ({m})")),
        }
    }
    Ok(out)
}

/// The judging half of the guard (`kith_guard.must_ask_rule`, `sentence_class`) is still the Python's. This hands it the
/// package and gets the problems back, and only when `settings.json` has rules for it to judge.
const MUST_ASK: &str = r"
import json, os, re, sys
from pathlib import Path
KITH = Path(sys.argv[1]); home = Path(sys.argv[2]); life_dir = sys.argv[3]
sys.path.insert(0, str(KITH / 'lib'))
from kith_paths import paths
import kith_guard
try:
    s = json.loads((home / 'settings.json').read_text())
except (OSError, ValueError):
    s = None
out = []
if s is not None:
    agent = home.parent.name if home.name == 'role' else home.name
    p = paths()
    values = {'KITH': str(KITH), 'HQ': str(KITH), 'AGENTS': str(p['KITH_AGENTS']), 'PACKAGE': str(home), 'AGENT': agent,
              'AGENT_DIR': str(home), 'HOME_DIR': life_dir, 'HOME_REAL': os.path.realpath(life_dir),
              'STATE': str(p['KITH_STATE']), 'MACHINE': str(p['KITH_MACHINE']), 'USER_HOME': str(Path.home())}
    fill = lambda t: re.sub(r'\{\{([A-Z_]+)\}\}', lambda m: values.get(m.group(1), m.group(0)), t)
    places = kith_guard.Places(package=home)
    perms = s.get('permissions') if isinstance(s.get('permissions'), dict) else {}
    for rule in perms.get('allow') or []:
        c = isinstance(rule, str) and kith_guard.must_ask_rule(rule, fill, places)
        if c:
            out.append(f'settings.json: allow {rule} is must-ask ({c}); the run asks for it whatever allows it')
    auto = s.get('autoMode') if isinstance(s.get('autoMode'), dict) else {}
    for line in auto.get('allow') or []:
        c = isinstance(line, str) and line != '$defaults' and kith_guard.sentence_class(line)
        if c:
            out.append(f'settings.json: autoMode.allow names a must-ask class ({c}): {line[:80]}')
print(json.dumps(out))
";

/// Whether `settings.json` has anything for the must-ask judge: a truthy `permissions.allow` or `autoMode.allow`.
fn has_rules(v: &Value) -> bool {
    let allow = |key: &str| {
        v.get(key)
            .filter(|x| x.is_object())
            .and_then(|x| x.get("allow"))
            .is_some_and(truthy)
    };
    allow("permissions") || allow("autoMode")
}

/// `must_ask_problems(home)`: rules a package declares that a must-ask class covers.
fn must_ask_problems(ctx: &Ctx, home: &Path) -> R<Vec<String>> {
    let Ok(text) = read_text(&home.join("settings.json")) else {
        return Ok(vec![]);
    };
    let Ok(s) = loads(&text) else {
        return Ok(vec![]);
    };
    if !s.is_object() {
        return Err(no_attr(&s, "get"));
    }
    if !has_rules(&s) {
        return Ok(vec![]);
    }
    let agent = if name_of(home) == "role" {
        home.parent().map(name_of).unwrap_or_default()
    } else {
        name_of(home)
    };
    let mut cmd = Command::new(&ctx.python);
    cmd.arg("-I")
        .arg("-c")
        .arg(MUST_ASK)
        .arg(&ctx.kernel)
        .arg(home)
        .arg(life(ctx, &agent));
    let ran = run(cmd, Duration::from_secs(60))?;
    if ran.code != 0 {
        return Err(PyErr::new(Kind::Other, super::py::strip(&ran.stderr)));
    }
    match serde_json::from_str::<Vec<String>>(&ran.stdout) {
        Ok(v) => Ok(v),
        Err(e) => Err(PyErr::new(Kind::Other, format!("must-ask judge: {e}"))),
    }
}
