//! The system prompt a launch assembles: a port of the prompt part of `kernel/bin/kith` (agent.md body and the memory
//! protocol with the placeholders filled, long-term memory through the load filter in `facets`, the `<session/>` tag
//! and, for anyone but the owner, the `<limits>` text). Pure: the person, the project and the folders are arguments.
//!
//! Left to the caller, because they need more than files: whether the person may run at all (`kith-auth resolve`), the
//! project the working folder belongs to (`project-id`, which asks git), and the guest's temp folder (`guest_tmp`, a
//! hash). The verb passes `--project` and the guest's temp note explicitly; the later `--launch-json` assembly supplies them.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::facets::{self, Kind};
use crate::paths::{self, Paths};

// ---- the project registry (project-id --info, --children --all)

/// `<state>/projects.json`: the parents and children of a project, by `bin/project-id`'s rules.
pub struct Registry(serde_json::Map<String, Value>);

impl Registry {
    pub fn load(state: &Path) -> Result<Self, String> {
        let f = state.join("projects.json");
        let Ok(text) = fs::read_to_string(&f) else {
            return Ok(Registry(serde_json::Map::new()));
        };
        let v: Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", f.display()))?;
        Ok(Registry(
            v.get("projects")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default(),
        ))
    }

    pub fn has(&self, id: &str) -> bool {
        self.0.contains_key(id)
    }

    pub fn known(&self) -> Vec<&str> {
        self.0.keys().map(String::as_str).collect()
    }

    /// The project and its parents, closest first; stops at a missing parent or a loop.
    pub fn chain(&self, id: &str) -> Vec<String> {
        let mut out: Vec<String> = vec![];
        let mut cur = id.to_owned();
        while self.0.contains_key(&cur) && !out.contains(&cur) {
            out.push(cur.clone());
            cur = self.0[&cur]
                .get("parent")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
        }
        out
    }

    /// The projects `id` holds and theirs, each after its parent, sorted.
    pub fn held(&self, id: &str) -> Vec<String> {
        fn go(r: &Registry, id: &str, seen: &mut Vec<String>) -> Vec<String> {
            let mut kids: Vec<&String> =
                r.0.iter()
                    .filter(|(q, p)| {
                        p.get("parent").and_then(Value::as_str) == Some(id) && !seen.contains(q)
                    })
                    .map(|(q, _)| q)
                    .collect();
            kids.sort();
            let mut out = vec![];
            for c in kids {
                if seen.contains(c) {
                    continue;
                }
                seen.push(c.clone());
                out.push(c.clone());
                out.extend(go(r, c, seen));
            }
            out
        }
        go(self, id, &mut vec![id.to_owned()])
    }
}

// ---- who the run is for (the part of `kith-auth resolve` the prompt uses)

/// The run's person as the prompt sees them. `kith-auth resolve` decides whether they may run at all; this only reads
/// who they are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Who {
    pub owner: bool,
    /// Their id, `""` for the owner when no person is named.
    pub id: String,
    /// The attributes the `<session/>` tag carries after the date, with their leading space (`""` for no person).
    pub line: String,
    pub rooms: String,
}

impl Who {
    pub fn owner() -> Self {
        Who {
            owner: true,
            id: String::new(),
            line: String::new(),
            rooms: String::new(),
        }
    }

    pub fn kind(&self) -> Kind {
        if self.id.starts_with("bot-") {
            Kind::Guest
        } else if self.owner {
            Kind::Owner
        } else {
            Kind::Person
        }
    }

    /// `person` is `KITH_PERSON`; `auth` is `auth.json` (`None` when it is missing). An unknown person is not the owner.
    pub fn resolve(auth: Option<&Value>, person: Option<&str>) -> Self {
        let Some(person) = person.filter(|p| !p.is_empty()) else {
            return Self::owner();
        };
        let empty = serde_json::Map::new();
        let people = auth
            .and_then(|a| a.get("people"))
            .and_then(Value::as_object)
            .unwrap_or(&empty);
        let id = if people.contains_key(person) {
            person.to_owned()
        } else {
            people
                .iter()
                .find(|(_, v)| {
                    v.get("was")
                        .and_then(Value::as_array)
                        .is_some_and(|w| w.iter().any(|x| x == person))
                })
                .map_or_else(|| person.to_owned(), |(k, _)| k.clone())
        };
        let owner_id = people
            .iter()
            .find(|(_, v)| v.get("owner").is_some_and(truthy))
            .map(|(k, _)| k.as_str());
        let owner = owner_id == Some(id.as_str());
        let me = people.get(&id);
        // A name goes into the <session/> tag, so anyone but the owner loses what could close or forge a tag.
        let clean = |s: &str| -> String {
            if owner {
                s.replace('"', "")
            } else {
                s.chars()
                    .filter(|c| !matches!(c, '<' | '>' | '"' | '\u{0}'..='\u{1f}' | '\u{7f}'))
                    .collect()
            }
        };
        let mut attrs = vec![format!("person=\"{}\"", clean(&id))];
        for k in ["name", "lang", "tz"] {
            if let Some(v) = me.and_then(|m| m.get(k)).filter(|v| truthy(v)) {
                let s = v.as_str().map_or_else(|| v.to_string(), str::to_owned);
                attrs.push(format!("{k}=\"{}\"", clean(&s)));
            }
        }
        Who {
            owner,
            id,
            line: format!(" {}", attrs.join(" ")),
            rooms: String::new(),
        }
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::String(s) => !s.is_empty(),
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

// ---- the agent's folders

/// The agent's package folder (the one holding agent.md): `<agents>/<name>/role`, `<agents>/<name>`, then the kernel's own.
pub fn agent_dir(p: &Paths, kernel: &Path, agent: &str) -> Option<PathBuf> {
    [
        p.agents.join(agent).join("role"),
        p.agents.join(agent),
        kernel.join("agents").join(agent),
    ]
    .into_iter()
    .find(|d| d.join("agent.md").is_file())
}

/// Claude Code's own temp folder for a guest, as `kith_paths.py guest_tmp` decides it: `<machine>/run/t/<id>`, or the
/// first 8 hex digits of `hash(id)` when that is too long, `None` when even that does not fit 44 bytes with
/// `/claude-<uid>`. `machine` is the real path; `hash` is the SHA-256 hex of its argument (kept out of this crate).
pub fn guest_tmp(
    machine_real: &Path,
    id: &str,
    uid: u32,
    hash: &dyn Fn(&str) -> String,
) -> Option<PathBuf> {
    let base = machine_real.join("run").join("t");
    let short = hash(id)[..8].to_owned();
    [id.to_owned(), short]
        .into_iter()
        .find(|name| format!("{}/{name}/claude-{uid}", base.display()).len() <= 44)
        .map(|name| base.join(name))
}

/// The sentence a guest's prompt carries when `guest_tmp` found no folder.
pub fn ctmp_note(machine: &Path, id: &str) -> String {
    format!(
        "Claude Code's temp folder for {id} doesn't fit in 44 bytes under {}/run/t, so it falls back to /tmp, which this run can't write: every shell command will fail. The owner can move this Mac's Kith folder to a shorter path.",
        machine.display()
    )
}

// ---- the prompt

/// Everything the prompt reads that is not a file under the agent's folders.
pub struct Input<'a> {
    pub agent: &'a str,
    pub paths: &'a Paths,
    /// The kernel's folder (`$KITH`): PROTOCOL.md and the `{{KITH}}` placeholder.
    pub kernel: &'a Path,
    /// The agent's live package folder (`$src_pkg`); `pkg` the pinned snapshot, or the same when running the live folder.
    pub src_pkg: &'a Path,
    pub pkg: &'a Path,
    pub who: &'a Who,
    /// The project the run is in (`""` for none), looked up in `registry`.
    pub project: &'a str,
    pub registry: &'a Registry,
    /// `$PWD` and `date +%F`.
    pub cwd: &'a str,
    pub date: &'a str,
    /// The sentence for `<limits>` when a guest's temp folder does not fit (`""` when it does).
    pub ctmp_note: &'a str,
    /// The user's home folder (`{{USER_HOME}}`).
    pub user_home: &'a Path,
    pub env: &'a dyn Fn(&str) -> Option<String>,
}

/// The agent.md body without its frontmatter, as the `awk` of `kernel/bin/kith` prints it: every line, newline-ended.
fn body(text: &str) -> String {
    let mut out = String::new();
    let mut fm = false;
    for (i, line) in text.split_terminator('\n').enumerate() {
        if i == 0 && line == "---" {
            fm = true;
        } else if fm && line == "---" {
            fm = false;
        } else if !fm {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn read_lossy(p: &Path) -> Option<String> {
    fs::read(p)
        .ok()
        .map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// The prompt template: the pinned `prompt.md`, else the live agent.md body, a blank line and PROTOCOL.md.
fn template(i: &Input<'_>) -> String {
    if let Some(t) = read_lossy(&i.pkg.join("prompt.md")) {
        return t;
    }
    let md = read_lossy(&i.src_pkg.join("agent.md")).unwrap_or_default();
    let mut s = body(&md);
    s.push('\n'); // the `echo` between the body and the protocol
    s.push_str(&read_lossy(&i.kernel.join("PROTOCOL.md")).unwrap_or_default());
    s
}

/// The folders of the run: `(home, memory, files, person file, work)`.
pub struct Dirs {
    pub home: PathBuf,
    pub mem: PathBuf,
    pub files: PathBuf,
    pub person_file: PathBuf,
    pub work: PathBuf,
}

pub fn dirs(i: &Input<'_>) -> Dirs {
    let home = if i.src_pkg.file_name().is_some_and(|n| n == "role") {
        i.src_pkg
            .parent()
            .map_or_else(|| i.src_pkg.to_path_buf(), Path::to_path_buf)
    } else {
        i.paths.state.join("agents").join(i.agent)
    };
    if i.who.owner {
        Dirs {
            mem: home.join("memory"),
            files: home.join("files"),
            person_file: i.paths.state.join("user.md"),
            work: PathBuf::new(),
            home,
        }
    } else {
        let files = home.join("files/people").join(&i.who.id);
        Dirs {
            mem: home.join("memory/people").join(&i.who.id),
            work: files.join(".work"),
            files,
            person_file: i
                .paths
                .state
                .join("people")
                .join(format!("{}.md", i.who.id)),
            home,
        }
    }
}

/// The system prompt, byte for byte what `kith AGENT --launch-json` carries as `system_prompt`.
pub fn build(i: &Input<'_>) -> Result<String, String> {
    if !i.project.is_empty() && !i.registry.has(i.project) {
        return Err(format!(
            "kith: unknown project '{}'. Known: {}",
            i.project,
            i.registry.known().join(" ")
        ));
    }
    let plain = |id: &str| {
        id.starts_with(|c: char| c.is_ascii_lowercase())
            && id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    };
    if !i.who.owner && !plain(&i.who.id) {
        return Err(format!("kith: bad person id '{}' from kith-auth", i.who.id));
    }
    let d = dirs(i);
    let home_real = paths::realpath(&d.home);
    let s = |p: &Path| p.to_string_lossy().into_owned();
    let shared = d.home.join("memory/shared");
    let fills: [(&str, String); 15] = [
        ("{{MEMORY_DIR}}", s(&d.mem)),
        ("{{SHARED_MEMORY}}", s(&shared)),
        ("{{FILES_DIR}}", s(&d.files)),
        ("{{PERSON_FILE}}", s(&d.person_file)),
        ("{{AGENT_DIR}}", s(i.pkg)),
        ("{{HOME_DIR}}", s(&d.home)),
        ("{{HOME_REAL}}", s(&home_real)),
        ("{{STATE}}", s(&i.paths.state)),
        ("{{MACHINE}}", s(&i.paths.machine)),
        ("{{AGENTS}}", s(&i.paths.agents)),
        ("{{PACKAGE}}", s(i.src_pkg)),
        ("{{USER_HOME}}", s(i.user_home)),
        ("{{KITH}}", s(i.kernel)),
        ("{{HQ}}", s(i.kernel)),
        ("{{AGENT}}", i.agent.to_owned()),
    ];
    let mut text = template(i);
    for (k, v) in &fills {
        text = text.replace(k, v);
    }
    // $(...) drops trailing newlines
    let text = text.trim_end_matches('\n');

    let kind = i.who.kind();
    let load = facets::load(
        &facets::Request {
            agent: i.agent,
            home: &d.home,
            person_file: &d.person_file,
            kind,
            person: &i.who.id,
            rooms: &i.who.rooms,
            room: &(i.env)("KITH_ROOM").unwrap_or_default(),
            project: i.project,
            agent_md: &i.src_pkg.join("agent.md"),
            state: &i.paths.state,
        },
        i.env,
    );
    let (parents, held) = if i.project.is_empty() {
        (vec![], vec![])
    } else {
        (
            i.registry.chain(i.project).split_off(1),
            i.registry.held(i.project),
        )
    };
    let mut tag = format!(
        "<session project=\"{}\"{}",
        if i.project.is_empty() {
            "none"
        } else {
            i.project
        },
        load.attrs
    );
    if !parents.is_empty() {
        tag.push_str(&format!(" parents=\"{}\"", parents.join(" ")));
    }
    if !held.is_empty() {
        tag.push_str(&format!(" holds=\"{}\"", held.join(" ")));
    }
    tag.push_str(&format!(
        " cwd=\"{}\" date=\"{}\"{}/>",
        i.cwd, i.date, i.who.line
    ));
    let mut prompt = format!("{text}\n{}\n{tag}", load.block);
    // A guest's run is told its limits up front, so it doesn't learn them one refused command at a time.
    if !i.who.owner {
        let note = if i.ctmp_note.is_empty() {
            String::new()
        } else {
            format!(" {}", i.ctmp_note)
        };
        prompt.push_str(&format!(
            "\n\n<limits>This run is in a sandbox for {id}. You can write only in {files} (their files; tool caches in {work}),\n{mem} (their memory) and $TMPDIR (temp files). /tmp, the owner's folders and other agents' folders are closed.\nShell commands inside the sandbox run without asking. Anything past it asks the owner: a website or a file outside\nthese folders with the request_access tool, another agent with the ask_helper tool (agent and task); `kith ask`\ndoesn't work from your shell.{note}</limits>",
            id = i.who.id,
            files = s(&d.files),
            work = s(&d.work),
            mem = s(&d.mem),
        ));
    }
    Ok(prompt)
}
