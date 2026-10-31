//! Who is asking, for a run that is not the owner's.
//!
//! An invited person's (or a bot guest's) agent run holds `KITH_RUN_TOKEN`, a random per-run secret the host issues
//! at launch and keeps only in memory, bound to the person, agent and room it was issued for. `kith recall` inside
//! such a run does not read the memory index and does not believe `KITH_PERSON` (an env var the run can change);
//! it asks the host over a unix socket (`KITH_RUN_SOCKET`) with the token. The host runs the kernel's own
//! `kith recall` as the identity the token names, so forging the env var gains nothing.
//!
//! One request per connection: a JSON line `{"token", "method": "recall", "args": [..]}`, answered by one JSON line
//! `{"ok": true, "stdout", "stderr", "code"}` or `{"ok": false, "error"}`.
//!
//! A second method, `requestPermission` (`{"token", "method": "requestPermission", "tool", "input"}`), is the host half of
//! Claude Code's `--permission-prompt-tool`: the run asks the host whether it may do something its limits would refuse,
//! the host makes a card for the approver (see `cards`) and answers `{"ok": true, "behavior": "allow"|"deny", "message"}`
//! after a short live window. It is open to a guest's token and to an owner token (`TokenScope::OwnerAsk`), which
//! carries no recall audience at all: `recall` with it is refused.
//!
//! Two more methods belong to the guest sandbox's way out (F11 step 2b, `cards::access`): `requestAccess` (`{"token",
//! "method": "requestAccess", "kind": "path"|"domain", "target", "reason"}`) makes an escape card and answers at once
//! `{"ok": true, "carded": true, "message"}` or `{"ok": false, "error"}`; `consumeAccess` (`{"token", "method"}`) is the
//! kernel's call at a guest run's launch and returns `{"ok": true, "grants": {"read", "write", "domains"}}`, once per run.
//! Both are for a guest token only.
//!
//! `requestSecret` (`{"token", "method": "requestSecret", "service", "purpose"}`) is the owner run's way to ask for a
//! service's key (`cards::secret`): it makes a card in the owner's chat and answers at once `{"ok": true, "carded":
//! true, "message"}` or `{"ok": false, "error"}`. An owner token only (`TokenScope::OwnerAsk`); a guest's is "not authorized".
//!
//! Two more read-only methods give a guest the kernel tools its sandbox keeps it from running itself: `memorySearch`
//! (`kith-memory search`, as the token's person and agent) and `projectId` (`project-id`: the project the run was
//! launched in, from the token, and `--info` of that one). Guest tokens only, same answer shape as `recall`.
//!
//! The token also has to come from the run it was issued to: on macOS a process of the same user can read another
//! process's environment (`ps eww` shows a Python or Node child's), so a token alone could be lifted from a
//! neighbouring run. The peer pid of the connection must descend from the pid the run's process was started as.

use crate::LockExt as _;
use anyhow::{Context, Result};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader};

pub const TOKEN_ENV: &str = "KITH_RUN_TOKEN";
pub const SOCKET_ENV: &str = "KITH_RUN_SOCKET";
/// The owner's runs get a token of their own kind under their own names: `KITH_RUN_TOKEN` in the environment means "not
/// the owner" to `kith recall`, and an owner token must not change that.
pub const ASK_TOKEN_ENV: &str = "KITH_ASK_TOKEN";
pub const ASK_SOCKET_ENV: &str = "KITH_ASK_SOCKET";
/// An owner token handed out over the API lives this long at most (a run is far shorter).
pub const ASK_TOKEN_TTL: Duration = Duration::from_secs(6 * 60 * 60);

/// A request line longer than this is refused unread.
const MAX_REQUEST: usize = 64 * 1024;
const MAX_ARGS: usize = 64;
const MAX_ARG_LEN: usize = 4096;
const MAX_OUTPUT: usize = 256 * 1024;
const READ_BUDGET: Duration = Duration::from_secs(5);
const RECALL_BUDGET: Duration = Duration::from_secs(30);
/// How far up the process tree a peer is followed before it counts as not descending.
const MAX_HOPS: usize = 64;

/// What a token may ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenScope {
    /// An invited person's or a bot guest's run: `recall` as them, and cards.
    Guest,
    /// The owner's run: cards only. The owner reads the index directly; this token reads nothing.
    OwnerAsk,
}

/// Whom a token stands for. Everything the host decides about a request comes from here, never from its arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunIdentity {
    pub person: String,
    pub agent: String,
    pub room: Option<String>,
    pub run_id: String,
    pub scope: TokenScope,
    /// What a card of this run belongs to (A6, C8): fixed when the token is issued, never taken from a request.
    pub session: String,
    /// The project the run was launched in (`project-id` answers about this one only): fixed when the token is issued,
    /// never taken from a request.
    pub project: Option<String>,
    /// The `kith ask` this run answers (an owner run only): its cards carry it, so a card it parks on answers that ask.
    pub reply: Option<AskReply>,
}

/// The `kith ask` a run was started by: its id (the asker's mailbox) and the agent that asked, none for a terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskReply {
    pub to: String,
    pub asked_by: Option<String>,
}

impl AskReply {
    /// An ask id and asking agent as the kernel writes them (kith-ask, kith-permit): `None` for an id that isn't one; an
    /// asker that isn't an agent name is a terminal.
    pub fn parse(to: &str, asked_by: Option<&str>) -> Option<Self> {
        let id_char = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-');
        if to.is_empty() || to.len() > 64 || !to.chars().all(id_char) {
            return None;
        }
        let agent = asked_by.filter(|a| {
            a.len() <= 64
                && a.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
                && a.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
        });
        Some(Self { to: to.to_owned(), asked_by: agent.map(str::to_owned) })
    }
}

/// What the host learned about the connection's other end.
#[derive(Debug, Clone, Copy, Default)]
pub struct PeerInfo {
    pub pid: Option<u32>,
}

struct Entry {
    identity: RunIdentity,
    /// The process the run was started as; the peer must be it or below it. Not bound yet: nobody matches.
    pid: Option<u32>,
    /// Tokens handed out over the API have an end; a run's own ends with its process.
    expires: Option<Instant>,
}

/// The live tokens, by the SHA-256 of the token (the map never holds a token itself).
pub struct TokenTable {
    entries: Mutex<HashMap<[u8; 32], Entry>>,
    /// `(peer, ancestor)`: whether `peer` is `ancestor` or one of its descendants. Replaceable for tests.
    ancestry: fn(u32, u32) -> bool,
}

/// Whether a process is still there.
pub fn pid_alive(pid: u32) -> bool {
    std::process::Command::new("ps")
        .args(["-p", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn digest(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

impl TokenTable {
    pub fn new() -> Self {
        Self { entries: Mutex::new(HashMap::new()), ancestry: descends }
    }

    #[cfg(test)]
    fn with_ancestry(ancestry: fn(u32, u32) -> bool) -> Self {
        Self { entries: Mutex::new(HashMap::new()), ancestry }
    }

    /// A fresh token for `identity`: 256 random bits as hex.
    fn insert(&self, identity: RunIdentity) -> Result<(String, [u8; 32])> {
        let mut raw = [0u8; 32];
        getrandom::fill(&mut raw).map_err(|_| anyhow::anyhow!("random bytes unavailable"))?;
        let token = raw.iter().fold(String::with_capacity(64), |mut acc, b| {
            let _ = write!(acc, "{b:02x}");
            acc
        });
        let key = digest(&token);
        self.entries.locked().insert(key, Entry { identity, pid: None, expires: None });
        Ok((token, key))
    }

    fn bind(&self, key: &[u8; 32], pid: u32) {
        if let Some(e) = self.entries.locked().get_mut(key) {
            e.pid = Some(pid);
        }
    }

    fn revoke(&self, key: &[u8; 32]) {
        self.entries.locked().remove(key);
    }

    /// A token for an owner run the kernel starts itself (over the API): bound to the process that will start the
    /// run, ends at `ttl`. Not tied to a [`RunGrant`]; [`Self::reap`] and [`Self::revoke_token`] end it.
    pub fn insert_ask(&self, identity: RunIdentity, pid: u32, ttl: Duration) -> Result<String> {
        let (token, key) = self.insert(identity)?;
        if let Some(e) = self.entries.locked().get_mut(&key) {
            e.pid = Some(pid);
            e.expires = Some(Instant::now() + ttl);
        }
        Ok(token)
    }

    /// Ends a token handed out by [`Self::insert_ask`]; false when it was not there.
    pub fn revoke_token(&self, token: &str) -> bool {
        self.entries.locked().remove(&digest(token)).is_some()
    }

    /// Drops API-issued tokens past their end or whose process is gone (`alive`); returns the run ids that lost one,
    /// so their cards can park (C6). A run's own token ends with its [`RunGrant`] instead. `alive` may be slow (it
    /// asks `ps`), so it runs on a snapshot of the pids with the table unlocked; call this off the async workers.
    pub fn reap(&self, alive: fn(u32) -> bool) -> Vec<String> {
        let now = Instant::now();
        let snapshot: Vec<([u8; 32], String, Option<u32>, bool)> = self
            .entries
            .locked()
            .iter()
            .filter(|(_, e)| e.expires.is_some())
            .map(|(k, e)| (*k, e.identity.run_id.clone(), e.pid, e.expires.is_some_and(|t| t <= now)))
            .collect();
        let dead: Vec<([u8; 32], String)> = snapshot
            .into_iter()
            .filter(|(_, _, pid, expired)| *expired || pid.is_some_and(|p| !alive(p)))
            .map(|(k, run, _, _)| (k, run))
            .collect();
        let mut entries = self.entries.locked();
        dead.into_iter().filter_map(|(k, run)| entries.remove(&k).map(|_| run)).collect()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.locked().len()
    }

    /// The one place a token becomes an identity: the token must be live and the peer must be the run's own process
    /// or below it. Unknown, revoked, unbound and foreign peers are all `None`.
    pub fn resolve(&self, token: &str, peer: PeerInfo) -> Option<RunIdentity> {
        let (identity, pid) = {
            let entries = self.entries.locked();
            let e = entries.get(&digest(token))?;
            if e.expires.is_some_and(|t| t <= Instant::now()) {
                return None;
            }
            (e.identity.clone(), e.pid?)
        };
        let peer = peer.pid?;
        (self.ancestry)(peer, pid).then_some(identity)
    }
}

impl Default for TokenTable {
    fn default() -> Self {
        Self::new()
    }
}

/// A token's lifetime: revoked when this is dropped, which is when the run's process is gone.
pub struct RunGrant {
    table: Arc<TokenTable>,
    key: [u8; 32],
    token: String,
    run_id: String,
    scope: TokenScope,
}

impl RunGrant {
    /// Issues a token on `table`.
    pub fn issue(table: &Arc<TokenTable>, identity: RunIdentity) -> Result<Self> {
        let (run_id, scope) = (identity.run_id.clone(), identity.scope);
        let (token, key) = table.insert(identity)?;
        Ok(Self { table: table.clone(), key, token, run_id, scope })
    }

    /// Binds the token to the process the run was started as. Until then no request resolves.
    pub fn bind_pid(&self, pid: u32) {
        self.table.bind(&self.key, pid);
    }

    /// The run's id: what its requests are known by (a once grant reserved for its launch, the cards it parks).
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// The environment the run gets. The only place the token leaves this module.
    pub fn env(&self, socket: &Path) -> [(String, String); 2] {
        let (token, sock) = match self.scope {
            TokenScope::Guest => (TOKEN_ENV, SOCKET_ENV),
            TokenScope::OwnerAsk => (ASK_TOKEN_ENV, ASK_SOCKET_ENV),
        };
        [(token.into(), self.token.clone()), (sock.into(), socket.to_string_lossy().into_owned())]
    }
}

impl Drop for RunGrant {
    fn drop(&mut self) {
        self.table.revoke(&self.key);
        // The process is gone: what it was waiting on parks (C6).
        if let Some(hub) = hub() {
            crate::cards::run_exited(&hub, &self.run_id);
        }
    }
}

static TABLE: OnceLock<Arc<TokenTable>> = OnceLock::new();
static SOCKET: OnceLock<PathBuf> = OnceLock::new();
static HUB: OnceLock<std::sync::Weak<crate::hub::Hub>> = OnceLock::new();

/// Lets the socket reach the card store. Before this, `requestPermission` answers no.
pub fn attach(hub: &Arc<crate::hub::Hub>) {
    let _ = HUB.set(Arc::downgrade(hub));
}

pub(crate) fn hub() -> Option<Arc<crate::hub::Hub>> {
    HUB.get()?.upgrade()
}

/// The socket path once it is listening.
pub fn socket() -> Option<&'static Path> {
    SOCKET.get().map(PathBuf::as_path)
}

pub fn table() -> &'static Arc<TokenTable> {
    TABLE.get_or_init(|| Arc::new(TokenTable::new()))
}

/// A grant for a run of `person` (an invited person's or a bot guest's id) on `agent`, with the environment to give
/// it. `None` when the socket isn't listening: such a run gets no token and no socket, and its recall reads nothing.
pub fn grant_for(
    person: &str,
    agent: &str,
    room: Option<&str>,
    session: &str,
    project: Option<&str>,
) -> Option<(RunGrant, [(String, String); 2])> {
    let socket = SOCKET.get()?;
    let identity = RunIdentity {
        person: person.to_owned(),
        agent: agent.to_owned(),
        room: room.filter(|r| !r.is_empty()).map(str::to_owned),
        run_id: uuid::Uuid::new_v4().simple().to_string(),
        scope: TokenScope::Guest,
        session: session_of(person, session),
        project: project.filter(|p| !p.is_empty()).map(str::to_owned),
        reply: None,
    };
    match RunGrant::issue(table(), identity) {
        Ok(grant) => {
            let env = grant.env(socket);
            Some((grant, env))
        }
        Err(error) => {
            tracing::warn!(error = format!("{error:#}"), "no run token issued");
            None
        }
    }
}

/// The session a guest -p run's cards belong to: the person and the conversation (thread or Claude session) it
/// continues, so another thread of one chat can't use a once grant. The `run:g:` and `run:o:` prefixes (and `acp:`)
/// can't overlap, whatever a person id or a session id looks like.
fn session_of(person: &str, session: &str) -> String {
    format!("run:g:{}:{person}:{session}", person.len())
}

/// The identity of an owner run of `agent` under `session` (cards only, see [`TokenScope::OwnerAsk`]), answering the
/// `kith ask` in `reply` when it was started by one.
pub fn ask_identity(agent: &str, session: &str, reply: Option<AskReply>) -> RunIdentity {
    RunIdentity {
        person: String::new(),
        agent: agent.to_owned(),
        room: None,
        run_id: uuid::Uuid::new_v4().simple().to_string(),
        scope: TokenScope::OwnerAsk,
        session: format!("run:o:{session}"),
        project: None,
        reply,
    }
}

/// An owner token (cards only) for a run of `agent` under `session`, with the environment to give it. `None` when the
/// socket isn't listening.
pub fn owner_ask_grant(
    agent: &str,
    session: &str,
    reply: Option<AskReply>,
) -> Option<(RunGrant, [(String, String); 2])> {
    let socket = SOCKET.get()?;
    let grant = RunGrant::issue(table(), ask_identity(agent, session, reply)).ok()?;
    let vars = grant.env(socket);
    Some((grant, vars))
}

/// For a run started from an environment list (channel runs): when it names a person, issues a grant and adds the
/// token and socket to the list. The owner's list (no `KITH_PERSON`) gets an owner token (cards only) under its own
/// names. `session` is the conversation the run continues (the work key of a channel thread): a card and a once
/// grant belong to it. `reply` is the `kith ask` an owner run answers (a resumed run's, from its card).
pub fn grant_for_env(
    env: &mut Vec<(std::ffi::OsString, std::ffi::OsString)>,
    agent: &str,
    session: &str,
    reply: Option<AskReply>,
) -> Option<RunGrant> {
    let get = |name: &str| env.iter().rev().find(|(k, _)| k == name).and_then(|(_, v)| v.to_str()).map(str::to_owned);
    let (grant, vars) = if let Some(person) = get("KITH_PERSON").filter(|p| !p.is_empty()) {
        grant_for(&person, agent, get("KITH_ROOM").as_deref(), session, get("KITH_PROJECT").as_deref())?
    } else {
        owner_ask_grant(agent, session, reply)?
    };
    env.extend(vars.into_iter().map(|(k, v)| (k.into(), v.into())));
    Some(grant)
}

/// `$KITH_MACHINE/run/host.sock`: outside the serve data folder, which a guest's sandbox denies.
///
/// The kernel's `KITH_MACHINE` (`kith_paths.py --json`) is the source of truth, because bin/kith allows exactly this
/// path in a guest's sandbox: ~/.kith on this Mac's own layout, ~/.config/kith under the old link layout. Without a
/// kernel to ask, the host's own reading of the same rules.
pub fn socket_path() -> PathBuf {
    let asked = crate::kernel::current_kernel().and_then(|k| {
        let out = std::process::Command::new("python3").arg(k.join("lib/kith_paths.py")).arg("--json").output().ok()?;
        out.status.success().then(|| machine_from_paths_json(&out.stdout))?
    });
    let machine = asked.unwrap_or_else(|| crate::kernel::machine_dir(&dirs::home_dir().unwrap_or_default()));
    machine.join("run").join("host.sock")
}

fn machine_from_paths_json(json: &[u8]) -> Option<PathBuf> {
    let v: Value = serde_json::from_slice(json).ok()?;
    v["KITH_MACHINE"].as_str().filter(|m| Path::new(m).is_absolute()).map(PathBuf::from)
}

// ---- requests

#[derive(Debug, PartialEq, Eq)]
pub enum Response {
    Ok {
        stdout: String,
        stderr: String,
        code: i32,
    },
    /// The answer to `requestPermission`.
    Decision {
        allow: bool,
        message: String,
        /// Denied for now: the card outlived the live window and the run is to end its turn on it.
        waiting: bool,
    },
    /// The answer to `requestAccess` and `consumeAccess`: the line as built.
    Line(Value),
    Refused(String),
}

impl Response {
    fn refused(why: &str) -> Self {
        Self::Refused(why.to_owned())
    }

    fn to_line(&self) -> String {
        let v = match self {
            Self::Ok { stdout, stderr, code } => json!({"ok": true, "stdout": stdout, "stderr": stderr, "code": code}),
            Self::Decision { allow, message, waiting } => {
                let mut v = json!({"ok": true, "behavior": if *allow { "allow" } else { "deny" }, "message": message});
                if *waiting {
                    v["waiting"] = true.into();
                }
                v
            }
            Self::Line(v) => v.clone(),
            Self::Refused(error) => json!({"ok": false, "error": error}),
        };
        format!("{v}\n")
    }
}

/// What a request is allowed to run: the identity (from the token alone) and the arguments `kith recall` is given.
#[derive(Debug, PartialEq, Eq)]
pub struct Authorized {
    pub identity: RunIdentity,
    /// Which kernel tool `args` are for.
    pub run: Run,
    pub args: Vec<String>,
    /// Set for `requestPermission`: what the run wants to do.
    pub ask: Option<Ask>,
    /// Set for `requestAccess` and `consumeAccess`.
    pub access: Option<AccessCall>,
}

/// The kernel tool a request runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Run {
    Recall,
    MemorySearch,
    /// No args: the run's own project id; `--info ID`: that project's chain.
    ProjectId,
    /// Not a tool run (a card or an access call).
    None,
}

/// The access methods of a run: the two guest-only ones, and `requestSecret` for an owner run.
#[derive(Debug, PartialEq, Eq)]
pub enum AccessCall {
    Request(Value),
    Consume,
    /// An owner run asks for a service's key (`{"service", "purpose"}`): a card in the owner's chat, `cards::secret`.
    Secret(Value),
}

/// What a run asks to do: a tool name and its input, as Claude Code's permission prompt tool is given them.
#[derive(Debug, PartialEq, Eq)]
pub struct Ask {
    pub tool: String,
    pub input: Value,
}

/// Checks a request line without running anything. Every token failure reads the same to the caller, so a probe
/// learns nothing about which tokens exist.
pub fn authorize(table: &TokenTable, line: &[u8], peer: PeerInfo) -> Result<Authorized, Response> {
    if line.len() > MAX_REQUEST {
        return Err(Response::refused("request too large"));
    }
    let Ok(req) = serde_json::from_slice::<Value>(line) else {
        return Err(Response::refused("malformed request"));
    };
    let (Some(token), Some(method)) = (req["token"].as_str(), req["method"].as_str()) else {
        return Err(Response::refused("malformed request"));
    };
    let Some(identity) = table.resolve(token, peer) else {
        return Err(Response::refused("not authorized"));
    };
    match method {
        "recall" => {
            let args = recall_args(req["args"].as_array().map_or(&[][..], Vec::as_slice))
                .map_err(|why| Response::Refused(why.to_owned()))?;
            // An owner token reads nothing: a guest's recall audience is neither widened by it nor given to it.
            if identity.scope != TokenScope::Guest {
                return Err(Response::refused("not authorized"));
            }
            Ok(Authorized { identity, run: Run::Recall, args, ask: None, access: None })
        }
        "memorySearch" | "projectId" => {
            // An owner token reads nothing here either.
            if identity.scope != TokenScope::Guest {
                return Err(Response::refused("not authorized"));
            }
            let given = req["args"].as_array().map_or(&[][..], Vec::as_slice);
            let (run, args) = if method == "memorySearch" {
                (Run::MemorySearch, memory_args(given))
            } else {
                (Run::ProjectId, project_args(given, identity.project.as_deref()))
            };
            let args = args.map_err(|why| Response::Refused(why.to_owned()))?;
            Ok(Authorized { identity, run, args, ask: None, access: None })
        }
        "requestPermission" => {
            let (Some(tool), input) = (req["tool"].as_str(), &req["input"]) else {
                return Err(Response::refused("a tool is needed"));
            };
            if tool.len() > MAX_ARG_LEN || !(input.is_object() || input.is_null()) {
                return Err(Response::refused("bad request"));
            }
            Ok(Authorized {
                identity,
                run: Run::None,
                args: Vec::new(),
                ask: Some(Ask { tool: tool.to_owned(), input: input.clone() }),
                access: None,
            })
        }
        "requestAccess" | "consumeAccess" => {
            // An owner token has no sandbox to leave.
            if identity.scope != TokenScope::Guest {
                return Err(Response::refused("not authorized"));
            }
            let call = if method == "consumeAccess" {
                AccessCall::Consume
            } else {
                for k in ["kind", "target", "reason"] {
                    if req[k].as_str().is_none_or(|s| s.len() > MAX_ARG_LEN) {
                        return Err(Response::refused("bad request"));
                    }
                }
                AccessCall::Request(json!({"kind": req["kind"], "target": req["target"], "reason": req["reason"],
                                           "write": req["write"].as_bool() == Some(true)}))
            };
            Ok(Authorized { identity, run: Run::None, args: Vec::new(), ask: None, access: Some(call) })
        }
        "requestSecret" => {
            // The owner's run asks the owner for a key; a guest's never does (v1: only the owner's agents hold keys).
            if identity.scope != TokenScope::OwnerAsk {
                return Err(Response::refused("not authorized"));
            }
            let (Some(service), purpose) = (req["service"].as_str(), req["purpose"].as_str().unwrap_or_default())
            else {
                return Err(Response::refused("bad request"));
            };
            if service.len() > MAX_ARG_LEN || purpose.len() > MAX_ARG_LEN {
                return Err(Response::refused("bad request"));
            }
            let call = AccessCall::Secret(json!({"service": service, "purpose": purpose}));
            Ok(Authorized { identity, run: Run::None, args: Vec::new(), ask: None, access: Some(call) })
        }
        _ => Err(Response::refused("unknown method")),
    }
}

/// The flags of `kith recall` that only narrow a search (the kernel filters by the caller's audience first); who asks
/// is never an argument.
const VALUE_FLAGS: &[&str] = &["--project", "--room", "--thread", "--tier", "-n", "--limit"];

/// Turns the request's arguments into `kith recall`'s: the query words, `--json` and the narrowing flags above, nothing
/// else (a word starting with `-` is not a query). The result starts with a flag, so a query that begins with `index`
/// (or `forget`) can never reach a subcommand of `kith recall`.
pub fn recall_args(args: &[Value]) -> Result<Vec<String>, &'static str> {
    if args.len() > MAX_ARGS {
        return Err("too many arguments");
    }
    let mut words = Vec::new();
    let (mut json, mut tier, mut limit) = (false, "2".to_owned(), None::<String>);
    let mut flags: Vec<(String, String)> = Vec::new();
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let Some(arg) = arg.as_str().filter(|a| a.len() <= MAX_ARG_LEN) else { return Err("bad argument") };
        if arg == "--json" {
            json = true;
        } else if VALUE_FLAGS.contains(&arg) {
            let Some(value) = it.next().and_then(Value::as_str).filter(|v| v.len() <= MAX_ARG_LEN) else {
                return Err("a flag needs a value");
            };
            match arg {
                "--tier" => value.clone_into(&mut tier),
                "-n" | "--limit" => limit = Some(value.to_owned()),
                _ => flags.push((arg.to_owned(), value.to_owned())),
            }
        } else if arg.starts_with('-') {
            return Err("that flag isn't available here");
        } else {
            words.push(arg.to_owned());
        }
    }
    if words.is_empty() {
        return Err("a query is needed");
    }
    let mut out = vec!["--tier".to_owned(), tier];
    if let Some(n) = limit {
        out.extend(["-n".to_owned(), n]);
    }
    for (k, v) in flags {
        out.extend([k, v]);
    }
    if json {
        out.push("--json".into());
    }
    out.extend(words);
    Ok(out)
}

/// Turns the request's arguments into `kith-memory`'s: `search`, then `-n N` (clamped), `--json` and the query words.
/// Everything else is refused: `--all` and `--agent` (the agent is the token's), every other dash word. The result
/// starts with `search`, so no query word can reach another subcommand.
pub fn memory_args(args: &[Value]) -> Result<Vec<String>, &'static str> {
    if args.len() > MAX_ARGS {
        return Err("too many arguments");
    }
    let (mut words, mut json, mut limit) = (Vec::new(), false, None::<u32>);
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let Some(arg) = arg.as_str().filter(|a| a.len() <= MAX_ARG_LEN) else { return Err("bad argument") };
        if arg == "--json" {
            json = true;
        } else if arg == "-n" {
            let n = it.next().and_then(Value::as_str).filter(|v| !v.is_empty() && v.len() <= 6);
            let Some(n) = n.filter(|v| v.bytes().all(|b| b.is_ascii_digit())) else {
                return Err("-n needs a number");
            };
            limit = Some(n.parse::<u32>().map_err(|_| "-n needs a number")?.clamp(1, 50));
        } else if arg.starts_with('-') {
            return Err("that flag isn't available here");
        } else {
            words.push(arg.to_owned());
        }
    }
    if words.is_empty() {
        return Err("a query is needed");
    }
    let mut out = vec!["search".to_owned()];
    if let Some(n) = limit {
        out.extend(["-n".to_owned(), n.to_string()]);
    }
    if json {
        out.push("--json".into());
    }
    out.extend(words);
    Ok(out)
}

/// `project-id` for a guest: nothing (the run's own project), or `--info <that project>`. Every other verb, a `dir`
/// and another project's id are refused; `project` is the one fixed in the token. The result is empty or starts with
/// `--info`.
pub fn project_args(args: &[Value], project: Option<&str>) -> Result<Vec<String>, &'static str> {
    let args = args.iter().map(|a| a.as_str().filter(|a| a.len() <= MAX_ARG_LEN)).collect::<Option<Vec<_>>>();
    match args.as_deref() {
        None => Err("bad argument"),
        Some([]) => Ok(Vec::new()),
        Some(["--info", id]) if project == Some(*id) && !id.starts_with('-') => {
            Ok(vec!["--info".to_owned(), (*id).to_owned()])
        }
        Some(_) => Err("only this run's own project is available here"),
    }
}

/// Answers one request line: refuses it, or runs the kernel's `kith recall` as the token's identity.
pub async fn handle_request(table: &TokenTable, line: &[u8], peer: PeerInfo) -> Response {
    match authorize(table, line, peer) {
        Ok(Authorized { identity, ask: Some(ask), .. }) => match hub() {
            Some(hub) => {
                let d = crate::cards::request_permission(&hub, &identity, &ask.tool, &ask.input).await;
                Response::Decision { allow: d.allow, message: d.message, waiting: d.waiting }
            }
            None => Response::Decision {
                allow: false,
                message: "Approvals aren't available right now.".into(),
                waiting: false,
            },
        },
        Ok(Authorized { identity, access: Some(call), .. }) => match (hub(), call) {
            (Some(hub), AccessCall::Request(req)) => {
                Response::Line(crate::cards::access::request(&hub, &identity, &req).await)
            }
            (Some(hub), AccessCall::Consume) => Response::Line(crate::cards::access::consume(&hub, &identity).await),
            (Some(hub), AccessCall::Secret(req)) => {
                let origin = crate::cards::secret::Origin::run(&identity);
                let service = req["service"].as_str().unwrap_or_default();
                let purpose = req["purpose"].as_str().unwrap_or_default();
                Response::Line(crate::cards::secret::request(&hub, &origin, service, purpose).await)
            }
            (None, _) => Response::refused("Approvals aren't available right now."),
        },
        // The run's own project id is in its token: nothing to run, and nothing to guess from the host's folder.
        Ok(Authorized { run: Run::ProjectId, args, identity, .. }) if args.is_empty() => match identity.project {
            Some(id) => Response::Ok { stdout: format!("{id}\n"), stderr: String::new(), code: 0 },
            None => Response::Ok { stdout: String::new(), stderr: String::new(), code: 1 },
        },
        Ok(job) => run_kernel(&job).await,
        Err(refused) => refused,
    }
}

/// Env names the recall child must not inherit from the host: identity comes from the token only.
const SCRUB: &[&str] = &[
    "KITH_AGENT",
    "HQ_AGENT",
    "KITH_PERSON",
    "HQ_PERSON",
    "KITH_ROOM",
    "HQ_ROOM",
    "KITH_ROOM_MEMBERS",
    "KITH_ROOM_OWNER_ONLY",
    "KITH_PROJECT",
    "HQ_PROJECT",
    "KITH_VIA",
    "HQ_VIA",
    TOKEN_ENV,
    SOCKET_ENV,
    ASK_TOKEN_ENV,
    ASK_SOCKET_ENV,
];

/// The kernel tool next to the `kith` entry point (`kith recall` is a subcommand; the other two are programs of `bin/`).
async fn tool_path(bin: PathBuf, tool: &str) -> Option<PathBuf> {
    let tool = tool.to_owned();
    tokio::task::spawn_blocking(move || {
        let real = bin.canonicalize().unwrap_or(bin);
        Some(real.parent()?.join(tool)).filter(|p| p.is_file())
    })
    .await
    .ok()
    .flatten()
}

async fn run_kernel(job: &Authorized) -> Response {
    let Ok(Some(bin)) = tokio::task::spawn_blocking(crate::agent::hq::bin).await else {
        return Response::refused("kith isn't available on this computer");
    };
    let mut command = match job.run {
        Run::Recall => {
            let mut c = tokio::process::Command::new(bin);
            c.arg("recall");
            c
        }
        Run::MemorySearch | Run::ProjectId => {
            let tool = if job.run == Run::MemorySearch { "kith-memory" } else { "project-id" };
            let Some(path) = tool_path(bin, tool).await else { return Response::refused("that isn't available here") };
            tokio::process::Command::new(path)
        }
        Run::None => return Response::refused("unknown method"),
    };
    command.args(&job.args);
    for name in SCRUB {
        command.env_remove(name);
    }
    command.envs(crate::kernel::both("AGENT", &job.identity.agent));
    command.envs(crate::kernel::both("PERSON", &job.identity.person));
    command.envs(crate::kernel::both("VIA", "serve"));
    if let Some(room) = &job.identity.room {
        command.envs(crate::kernel::both("ROOM", room));
    }
    // The project's shared files are part of what a guest's memory search may read (kith-memory narrows to this one).
    if let (Run::MemorySearch, Some(project)) = (job.run, &job.identity.project) {
        command.envs(crate::kernel::both("PROJECT", project));
    }
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    command.kill_on_drop(true);
    let Ok(mut child) = command.spawn() else { return Response::refused("could not run that") };
    let (mut out, mut err) = (child.stdout.take().expect("piped"), child.stderr.take().expect("piped"));
    let work = async {
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let (mut out, mut err) = ((&mut out).take(MAX_OUTPUT as u64), (&mut err).take(MAX_OUTPUT as u64));
        let (a, b) = tokio::join!(out.read_to_end(&mut stdout), err.read_to_end(&mut stderr));
        a.and(b)?;
        // Output past the cap is left unread: a child blocked on its full pipe is cut off by the budget below.
        let status = child.wait().await?;
        Ok::<_, std::io::Error>((status, stdout, stderr))
    };
    match tokio::time::timeout(RECALL_BUDGET, work).await {
        Ok(Ok((status, stdout, stderr))) => Response::Ok {
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
            code: status.code().unwrap_or(1),
        },
        _ => Response::refused("timed out"),
    }
}

// ---- the socket

async fn serve_one(table: Arc<TokenTable>, stream: tokio::net::UnixStream) {
    let peer = PeerInfo { pid: stream.peer_cred().ok().and_then(|c| c.pid()).and_then(|p| u32::try_from(p).ok()) };
    let (read, mut write) = stream.into_split();
    let mut line = Vec::new();
    let read = tokio::time::timeout(READ_BUDGET, async {
        // One byte past the cap tells an oversized line from one that is exactly at it.
        BufReader::new(read.take(MAX_REQUEST as u64 + 1)).read_until(b'\n', &mut line).await
    })
    .await;
    let response = match read {
        Ok(Ok(n)) if n > 0 => {
            if line.last() == Some(&b'\n') {
                line.pop();
            }
            handle_request(&table, &line, peer).await
        }
        _ => Response::refused("malformed request"),
    };
    let _ = write.write_all(response.to_line().as_bytes()).await;
}

/// Binds the socket (`dir` 0700, the socket 0600, a stale file from an earlier host removed) and answers requests in
/// the background. A failure leaves the host running and runs without tokens (see [`grant_for`]).
pub fn start() {
    let path = socket_path();
    match bind(&path) {
        Ok(listener) => {
            let _ = SOCKET.set(path.clone());
            tracing::info!(socket = %path.display(), "run socket listening");
            let table = table().clone();
            tokio::spawn(async move {
                loop {
                    match listener.accept().await {
                        Ok((stream, _)) => {
                            tokio::spawn(serve_one(table.clone(), stream));
                        }
                        Err(error) => {
                            tracing::warn!(%error, "run socket accept failed");
                            tokio::time::sleep(Duration::from_millis(200)).await;
                        }
                    }
                }
            });
        }
        Err(error) => {
            tracing::warn!(error = format!("{error:#}"), "run socket not started: guests' recall stays empty");
        }
    }
}

fn bind(path: &Path) -> Result<tokio::net::UnixListener> {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = path.parent().context("socket path has no folder")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    // A file left by a host that didn't shut down cleanly; the host lock means no other host is using it.
    let _ = std::fs::remove_file(path);
    let listener = tokio::net::UnixListener::bind(path).with_context(|| format!("binding {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

// ---- process ancestry

/// Whether `pid` is `ancestor` or has it among its parents. Asks `ps`, which both macOS and Linux have; any failure
/// (a process gone, `ps` missing) is "no".
fn descends(pid: u32, ancestor: u32) -> bool {
    let mut cur = pid;
    for _ in 0..MAX_HOPS {
        if cur == ancestor {
            return true;
        }
        if cur <= 1 {
            return false;
        }
        let Ok(out) = std::process::Command::new("ps").args(["-o", "ppid=", "-p", &cur.to_string()]).output() else {
            return false;
        };
        let Some(parent) = String::from_utf8_lossy(&out.stdout).trim().parse::<u32>().ok() else { return false };
        cur = parent;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(person: &str) -> RunIdentity {
        RunIdentity {
            person: person.into(),
            agent: "echo".into(),
            room: Some("1:1:echo:sam".into()),
            run_id: "r1".into(),
            scope: TokenScope::Guest,
            session: "run:1:1:echo:sam".into(),
            project: Some("alpha".into()),
            reply: None,
        }
    }

    fn open_table() -> Arc<TokenTable> {
        Arc::new(TokenTable::with_ancestry(|peer, run| peer == run))
    }

    fn req(token: &str, method: &str, args: &Value) -> Vec<u8> {
        json!({"token": token, "method": method, "args": args}).to_string().into_bytes()
    }

    fn peer(pid: u32) -> PeerInfo {
        PeerInfo { pid: Some(pid) }
    }

    #[test]
    fn issue_lookup_revoke() {
        let t = open_table();
        let g = RunGrant::issue(&t, id("sam")).unwrap();
        let token = g.token.clone();
        assert_eq!(token.len(), 64, "256 bits as hex");
        assert!(token.bytes().all(|b| b.is_ascii_hexdigit()));
        assert_eq!(t.resolve(&token, peer(7)), None, "unbound: nobody matches");
        g.bind_pid(7);
        assert_eq!(t.resolve(&token, peer(7)), Some(id("sam")));
        assert_eq!(t.resolve(&token, peer(8)), None, "another process");
        assert_eq!(t.resolve(&token, PeerInfo::default()), None, "no peer pid");
        drop(g);
        assert_eq!(t.resolve(&token, peer(7)), None, "revoked with the run");
        assert_eq!(t.len(), 0);
    }

    #[test]
    fn tokens_are_distinct_and_the_table_holds_no_token() {
        let t = open_table();
        let (a, b) = (RunGrant::issue(&t, id("sam")).unwrap(), RunGrant::issue(&t, id("eve")).unwrap());
        assert_ne!(a.token, b.token);
        a.bind_pid(1);
        b.bind_pid(2);
        assert_eq!(t.resolve(&a.token, peer(1)).unwrap().person, "sam");
        assert_eq!(t.resolve(&b.token, peer(2)).unwrap().person, "eve");
        assert_eq!(t.resolve(&a.token, peer(2)), None, "one run's token from another run's process");
        assert!(!t.entries.locked().keys().any(|k| k.as_slice() == a.token.as_bytes()));
    }

    #[test]
    fn unknown_revoked_and_malformed_requests_are_refused() {
        let t = open_table();
        let g = RunGrant::issue(&t, id("sam")).unwrap();
        g.bind_pid(5);
        let ok = req(&g.token, "recall", &json!(["zebra"]));
        assert!(authorize(&t, &ok, peer(5)).is_ok());
        assert!(authorize(&t, &req("nope", "recall", &json!(["zebra"])), peer(5)).is_err());
        assert!(authorize(&t, &req(&g.token, "shell", &json!(["ls"])), peer(5)).is_err(), "unknown method");
        assert!(authorize(&t, &req(&g.token, "forget", &json!(["x"])), peer(5)).is_err(), "no write methods");
        assert!(authorize(&t, b"not json", peer(5)).is_err());
        assert!(authorize(&t, b"{\"method\":\"recall\"}", peer(5)).is_err(), "no token");
        assert!(authorize(&t, b"[1,2]", peer(5)).is_err());
        let big = req(&g.token, "recall", &json!(["x".repeat(MAX_REQUEST)]));
        assert_eq!(authorize(&t, &big, peer(5)), Err(Response::refused("request too large")));
        drop(g);
        assert!(authorize(&t, &ok, peer(5)).is_err(), "after the run is gone");
    }

    #[tokio::test]
    async fn handle_request_refuses_without_running_anything() {
        let t = open_table();
        let r = handle_request(&t, &req("nope", "recall", &json!(["zebra"])), peer(1)).await;
        assert_eq!(r, Response::refused("not authorized"));
        assert!(r.to_line().contains("\"ok\":false"));
    }

    #[test]
    fn the_audience_is_the_tokens_whatever_the_arguments_say() {
        let t = open_table();
        let g = RunGrant::issue(&t, id("sam")).unwrap();
        g.bind_pid(5);
        let line = req(
            &g.token,
            "recall",
            &json!(["zebra", "--project", "alpha", "--room", "1:1:echo:eve", "KITH_PERSON=kai", "--json"]),
        );
        let job = authorize(&t, &line, peer(5)).unwrap();
        assert_eq!(job.identity, id("sam"), "person, agent and room are the token's");
        // Narrowing flags pass as flags with their values; an env-looking word is just a query word.
        assert_eq!(
            job.args,
            ["--tier", "2", "--project", "alpha", "--room", "1:1:echo:eve", "--json", "zebra", "KITH_PERSON=kai"]
        );
        for bad in [
            json!(["zebra", "--person", "kai"]),
            json!(["zebra", "--agent", "other"]),
            json!(["--env", "KITH_PERSON=kai", "zebra"]),
            json!(["zebra", "--project"]),
            json!([]),
            json!(["--json"]),
            json!([1, 2]),
        ] {
            assert!(authorize(&t, &req(&g.token, "recall", &bad), peer(5)).is_err(), "{bad} should be refused");
        }
        for sub in ["index", "forget"] {
            let job = authorize(&t, &req(&g.token, "recall", &json!([sub, "rebuild"])), peer(5)).unwrap();
            assert_eq!(job.args[0], "--tier", "{sub} is a query word, never a subcommand");
        }
    }

    #[test]
    fn memory_search_args_are_a_fixed_search() {
        assert_eq!(
            memory_args(&[json!("zebra"), json!("-n"), json!("500"), json!("--json")]).unwrap(),
            ["search", "-n", "50", "--json", "zebra"]
        );
        assert_eq!(memory_args(&[json!("ls"), json!("index")]).unwrap(), ["search", "ls", "index"]);
        for bad in [
            json!(["zebra", "--all"]),
            json!(["zebra", "--agent", "other"]),
            json!(["zebra", "--agent=other"]),
            json!(["zebra", "-n"]),
            json!(["zebra", "-n", "x"]),
            json!(["zebra", "-n", "-1"]),
            json!(["-x", "zebra"]),
            json!([]),
            json!(["--json"]),
            json!([7]),
        ] {
            assert!(memory_args(bad.as_array().unwrap()).is_err(), "{bad} should be refused");
        }
    }

    #[test]
    fn project_id_is_the_runs_own_project_only() {
        let own = Some("alpha");
        assert_eq!(project_args(&[], own).unwrap(), Vec::<String>::new());
        assert_eq!(project_args(&[json!("--info"), json!("alpha")], own).unwrap(), ["--info", "alpha"]);
        for bad in [
            json!(["--list"]),
            json!(["--children", "alpha"]),
            json!(["--where", "alpha"]),
            json!(["--register", "alpha"]),
            json!(["--root"]),
            json!(["--info", "beta"]),
            json!(["--info"]),
            json!(["--info", "alpha", "x"]),
            json!(["/Users/kai/secret"]),
            json!(["."]),
            json!([1]),
        ] {
            assert!(project_args(bad.as_array().unwrap(), own).is_err(), "{bad} should be refused");
        }
        // A run with no project has nothing to be told about.
        assert!(project_args(&[json!("--info"), json!("alpha")], None).is_err());
        assert!(project_args(&[json!("--info"), json!("")], None).is_err());
    }

    #[test]
    fn an_owner_token_gets_neither_memory_search_nor_project_id() {
        let t = open_table();
        let token = t.insert_ask(ask_identity("echo", "s1", None), 5, Duration::from_secs(60)).unwrap();
        for (method, args) in [("memorySearch", json!(["zebra"])), ("projectId", json!([]))] {
            let got = authorize(&t, &req(&token, method, &args), peer(5));
            assert!(matches!(got, Err(Response::Refused(ref why)) if why == "not authorized"), "{method}: {got:?}");
        }
    }

    #[test]
    fn memory_search_and_project_id_are_the_tokens_and_the_forged_fields_are_ignored() {
        let t = open_table();
        let g = RunGrant::issue(&t, id("sam")).unwrap();
        g.bind_pid(5);
        let line = |method: &str, args: &Value| {
            let mut v: Value = serde_json::from_slice(&req(&g.token, method, args)).unwrap();
            v["project"] = json!("beta");
            v["person"] = json!("kai");
            v["agent"] = json!("other");
            serde_json::to_vec(&v).unwrap()
        };
        let job = authorize(&t, &line("memorySearch", &json!(["zebra"])), peer(5)).unwrap();
        assert_eq!(
            (job.run, job.args, job.identity),
            (Run::MemorySearch, vec!["search".into(), "zebra".into()], id("sam"))
        );
        let job = authorize(&t, &line("projectId", &json!(["--info", "alpha"])), peer(5)).unwrap();
        assert_eq!((job.run, job.identity.project.as_deref()), (Run::ProjectId, Some("alpha")));
        assert!(authorize(&t, &line("projectId", &json!(["--info", "beta"])), peer(5)).is_err());
    }

    /// A stand-in kernel whose tools print who they were run as.
    fn fake_kernel() -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("kr-kernel-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]));
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let script = "#!/bin/sh\necho \"$0|$KITH_AGENT|$KITH_PERSON|$KITH_ROOM|$KITH_PROJECT|$KITH_VIA|$*\"\n";
        for tool in ["kith", "kith-memory", "project-id"] {
            std::fs::write(bin.join(tool), script).unwrap();
            std::fs::set_permissions(bin.join(tool), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        bin.join("kith")
    }

    #[tokio::test]
    async fn the_host_runs_the_kernel_tools_as_the_tokens_identity() {
        let _one = crate::remote::account::ONE_AT_A_TIME.lock().await;
        let kith = fake_kernel();
        *crate::agent::hq::TEST_KERNEL_BIN.lock().unwrap() = Some(kith.clone());
        let t = open_table();
        let g = RunGrant::issue(&t, id("sam")).unwrap();
        g.bind_pid(5);
        let out = |r: Response| match r {
            Response::Ok { stdout, code, .. } => (stdout, code),
            other => panic!("{other:?}"),
        };
        let (stdout, code) =
            out(handle_request(&t, &req(&g.token, "memorySearch", &json!(["zebra", "-n", "3"])), peer(5)).await);
        assert_eq!(code, 0);
        // The tool is the kernel's own `bin/kith-memory` (the path is canonical, /private/var for /var on a Mac).
        assert!(stdout.contains("/bin/kith-memory|echo|sam|1:1:echo:sam|alpha|serve|search -n 3 zebra\n"), "{stdout}");
        let (stdout, _) =
            out(handle_request(&t, &req(&g.token, "projectId", &json!(["--info", "alpha"])), peer(5)).await);
        assert!(stdout.ends_with("|--info alpha\n") && stdout.contains("/project-id|echo|sam|"), "{stdout}");
        // Bare: answered from the token, no process, and not from wherever the host happens to be.
        assert_eq!(
            out(handle_request(&t, &req(&g.token, "projectId", &json!([])), peer(5)).await),
            ("alpha\n".into(), 0)
        );
        let none = RunIdentity { project: None, run_id: "r2".into(), ..id("sam") };
        let g2 = RunGrant::issue(&t, none).unwrap();
        g2.bind_pid(6);
        assert_eq!(
            out(handle_request(&t, &req(&g2.token, "projectId", &json!([])), peer(6)).await),
            (String::new(), 1)
        );
        // Refusals never run anything.
        let r = handle_request(&t, &req(&g.token, "memorySearch", &json!(["zebra", "--all"])), peer(5)).await;
        assert!(matches!(r, Response::Refused(_)));
        *crate::agent::hq::TEST_KERNEL_BIN.lock().unwrap() = None;
        let _ = std::fs::remove_dir_all(kith.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn the_socket_lives_under_the_kernels_machine_folder() {
        let kept = machine_from_paths_json(br#"{"KITH_MACHINE": "/Users/x/.config/kith", "KITH_STATE": "/s"}"#);
        assert_eq!(kept, Some(PathBuf::from("/Users/x/.config/kith")));
        assert_eq!(machine_from_paths_json(br#"{"KITH_MACHINE": "relative"}"#), None);
        assert_eq!(machine_from_paths_json(b"nope"), None);
        assert!(socket_path().ends_with("run/host.sock"));
    }

    #[test]
    fn recall_args_caps() {
        assert!(recall_args(&vec![json!("a"); MAX_ARGS + 1]).is_err());
        assert!(recall_args(&[json!("x".repeat(MAX_ARG_LEN + 1))]).is_err());
        assert_eq!(recall_args(&[json!("a"), json!("--limit"), json!("3")]).unwrap(), ["--tier", "2", "-n", "3", "a"]);
    }

    #[test]
    fn an_owner_token_can_ask_for_a_card_but_reads_nothing() {
        let t = open_table();
        let owner = ask_identity("echo", "s1", None);
        let token = t.insert_ask(owner.clone(), 5, Duration::from_secs(60)).unwrap();
        let line = json!({"token": token, "method": "requestPermission", "tool": "Bash", "input": {"command": "ls"}});
        let job = authorize(&t, line.to_string().as_bytes(), peer(5)).unwrap();
        assert_eq!(job.identity, owner);
        assert_eq!(job.ask, Some(Ask { tool: "Bash".into(), input: json!({"command": "ls"}) }));
        assert!(authorize(&t, &req(&token, "requestPermission", &json!([])), peer(5)).is_err(), "no tool, no ask");
        // Recall is a guest's: an owner token is refused it, whatever it asks for (the audience is not widened).
        assert_eq!(
            authorize(&t, &req(&token, "recall", &json!(["zebra"])), peer(5)),
            Err(Response::refused("not authorized"))
        );
        // Only from the process it was issued for, and not after it ends.
        assert!(authorize(&t, line.to_string().as_bytes(), peer(6)).is_err());
        assert!(t.revoke_token(&token) && !t.revoke_token(&token));
        assert!(authorize(&t, line.to_string().as_bytes(), peer(5)).is_err());
        // Its own environment names: `KITH_RUN_TOKEN` stays the sign of a guest to `kith recall`.
        let g = RunGrant::issue(&t, owner).unwrap();
        let env = g.env(Path::new("/s"));
        assert_eq!((env[0].0.as_str(), env[1].0.as_str()), (ASK_TOKEN_ENV, ASK_SOCKET_ENV));
    }

    #[test]
    fn only_an_owner_token_can_ask_for_a_key() {
        let t = open_table();
        let owner = ask_identity("echo", "s1", None);
        let token = t.insert_ask(owner.clone(), 5, Duration::from_secs(60)).unwrap();
        let line = json!({"token": token, "method": "requestSecret", "service": "github", "purpose": "pull requests"});
        let job = authorize(&t, line.to_string().as_bytes(), peer(5)).unwrap();
        assert_eq!(job.identity, owner);
        assert_eq!(job.access, Some(AccessCall::Secret(json!({"service": "github", "purpose": "pull requests"}))));
        let no_service = json!({"token": token, "method": "requestSecret"});
        assert_eq!(authorize(&t, no_service.to_string().as_bytes(), peer(5)), Err(Response::refused("bad request")));
        let long = json!({"token": token, "method": "requestSecret", "service": "x".repeat(MAX_ARG_LEN + 1)});
        assert!(authorize(&t, long.to_string().as_bytes(), peer(5)).is_err());
        // A guest's run (an invited person's or a bot guest's) is not authorized, whatever it sends.
        let g = RunGrant::issue(&t, id("sam")).unwrap();
        g.bind_pid(6);
        let guest = json!({"token": g.token, "method": "requestSecret", "service": "github", "purpose": "x"});
        assert_eq!(authorize(&t, guest.to_string().as_bytes(), peer(6)), Err(Response::refused("not authorized")));
    }

    #[test]
    fn sessions_are_per_conversation_and_their_prefixes_cannot_overlap() {
        // Another thread of one chat is another session (C8).
        assert_ne!(session_of("sam", "t1"), session_of("sam", "t2"));
        // A person whose id is `owner`, or whose id ends like another's session starts, is not the owner's run or
        // another person.
        assert_ne!(session_of("owner", "x"), ask_identity("echo", "x", None).session);
        assert_ne!(session_of("a", "b:c"), session_of("a:b", "c"));
        assert!(ask_identity("echo", "g:1:sam:x", None).session.starts_with("run:o:"));
        assert!(session_of("owner", "x").starts_with("run:g:"));
    }

    #[test]
    fn an_api_issued_token_ends_with_its_process_or_its_time() {
        let t = open_table();
        let (a, b) = (ask_identity("echo", "a", None), ask_identity("echo", "b", None));
        let (run_a, run_b) = (a.run_id.clone(), b.run_id.clone());
        let ta = t.insert_ask(a, 7, Duration::from_secs(60)).unwrap();
        let tb = t.insert_ask(b, 8, Duration::from_millis(0)).unwrap();
        assert_eq!(t.resolve(&tb, peer(8)), None, "past its time");
        assert_eq!(t.reap(|pid| pid == 7), vec![run_b], "the expired one goes, the live one stays");
        assert!(t.resolve(&ta, peer(7)).is_some());
        assert_eq!(t.reap(|_| false), vec![run_a], "its process is gone");
        assert_eq!(t.len(), 0);
    }

    #[test]
    fn ancestry_follows_real_parents() {
        let me = std::process::id();
        let mut child = std::process::Command::new("sleep").arg("5").spawn().unwrap();
        let pid = child.id();
        assert!(descends(pid, me), "a child descends from its parent");
        assert!(descends(me, me));
        assert!(!descends(me, pid), "not the other way round");
        assert!(!descends(pid, 1 << 30));
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(!descends(pid, me), "a gone process descends from nothing");
    }

    #[test]
    fn env_list_gets_a_token_only_with_a_person() {
        // No socket is listening in unit tests, so no token is issued: the list is left as it was.
        let mut owner = vec![("KITH_AGENT".into(), "echo".into())];
        assert!(grant_for_env(&mut owner, "echo", "w1", None).is_none());
        assert_eq!(owner.len(), 1);
        let mut guest = vec![("KITH_PERSON".into(), "sam".into())];
        assert!(grant_for_env(&mut guest, "echo", "w1", None).is_none());
        assert!(!guest.iter().any(|(k, _)| k == TOKEN_ENV));
    }

    #[tokio::test]
    async fn socket_round_trip_refuses_a_forged_token() {
        let dir = std::env::temp_dir().join(format!("kr-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]));
        let sock = dir.join("host.sock");
        let listener = bind(&sock).unwrap();
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
            assert_eq!(std::fs::metadata(&sock).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let t = open_table();
        let server = tokio::spawn({
            let t = t.clone();
            async move {
                let (stream, _) = listener.accept().await.unwrap();
                serve_one(t, stream).await;
            }
        });
        let mut c = tokio::net::UnixStream::connect(&sock).await.unwrap();
        c.write_all(&req("forged", "recall", &json!(["zebra"]))).await.unwrap();
        c.write_all(b"\n").await.unwrap();
        let mut reply = String::new();
        c.read_to_string(&mut reply).await.unwrap();
        let v: Value = serde_json::from_str(reply.trim()).unwrap();
        assert_eq!(v["ok"], false);
        server.await.unwrap();
        // A stale file from an earlier host is replaced.
        drop(bind(&sock).unwrap());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

// Property tests (method in the file); a child of this module so they reach `with_ancestry`.
#[cfg(test)]
#[path = "runauth_props.rs"]
mod runauth_props;
