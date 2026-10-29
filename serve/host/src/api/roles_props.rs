//! Property tests for `roles`: whatever the method, body, store and kith-auth answer, an invited person never gets
//! more than they were granted. The real `roles` functions run against a real store and a stand-in `kith-auth`
//! (a shell script answering from a table written per case), so the subprocess path is the product's own.
//! Method: `.claude/skills/invitee-access/SKILL.md`. Run only these: `cargo test roles_props`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::devices::NotAllowed;
use super::roles::{self, MEMBER_METHODS, UNPROVEN_METHODS};
use crate::agent::folder::{Change, Pick};
use crate::channels::hq::TEST_ENV;
use crate::hub::Hub;
use crate::remote::account::{ONE_AT_A_TIME, TEST_BIN};
use crate::remote::identity::Identity;
use crate::store::{BotConfig, BotGuest, BotState, EntryKind, Lane, Store};
use proptest::prelude::*;
use proptest::test_runner::{Config, TestCaseError, TestRunner};
use serde_json::{Value, json};
use std::cell::Cell;
use std::path::PathBuf;
use std::sync::Arc;

const PEOPLE: [&str; 2] = ["p1", "p2"];
const AGENTS: [&str; 2] = ["a1", "a2"];
const PROJECTS: [&str; 2] = ["pr1", "pr2"];

/// Cases per property; `PROPTEST_CASES` overrides it (the CI default keeps the module near ten seconds).
fn runner() -> TestRunner {
    let mut config = Config { failure_persistence: None, ..Config::default() };
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = 96;
    }
    TestRunner::new(config)
}

// ---- the stand-in kith-auth ----

/// What the stand-in prints for one (person, agent). `Fail` is "no answer" (exit 1, nothing printed).
#[derive(Clone, Debug)]
enum Ans {
    Fail,
    Garbage(usize),
    Denied(bool),
    Member,
    Guest,
    Other(usize),
    NoPreset,
    OwnerTrue,
}

const GARBAGE: [&str; 10] = [
    "",
    "not json",
    "[]",
    "null",
    "true",
    r#"{"allowed":"true"}"#,
    r#"{"allowed":1}"#,
    r#"{"allowed":true"#,
    r#"{"allowed":null,"preset":"member"}"#,
    r#"{"allowed":[true],"preset":"member"}"#,
];
const OTHER_PRESETS: [&str; 4] = ["viewer", "MEMBER", "member ", ""];

impl Ans {
    fn text(&self) -> Option<String> {
        Some(match self {
            Self::Fail => return None,
            Self::Garbage(i) => GARBAGE[*i % GARBAGE.len()].to_owned(),
            Self::Denied(with_member) => {
                json!({"allowed": false, "preset": with_member.then_some("member")}).to_string()
            }
            Self::Member => json!({"allowed": true, "preset": "member"}).to_string(),
            Self::Guest => json!({"allowed": true, "preset": "guest"}).to_string(),
            Self::Other(i) => json!({"allowed": true, "preset": OTHER_PRESETS[*i % OTHER_PRESETS.len()]}).to_string(),
            Self::NoPreset => json!({"allowed": true}).to_string(),
            // The owner never comes through `roles`; a person answered as owner is refused.
            Self::OwnerTrue => json!({"allowed": true, "owner": true, "preset": "member"}).to_string(),
        })
    }

    /// What kith-auth means, by label (not by re-parsing): a grant of any preset, never the owner.
    fn allowed(&self) -> bool {
        matches!(self, Self::Member | Self::Guest | Self::Other(_) | Self::NoPreset)
    }

    fn member(&self) -> bool {
        matches!(self, Self::Member)
    }
}

fn ans() -> impl Strategy<Value = Ans> {
    prop_oneof![
        2 => Just(Ans::Fail),
        2 => (0..GARBAGE.len()).prop_map(Ans::Garbage),
        2 => any::<bool>().prop_map(Ans::Denied),
        3 => Just(Ans::Member),
        2 => Just(Ans::Guest),
        1 => (0..OTHER_PRESETS.len()).prop_map(Ans::Other),
        1 => Just(Ans::NoPreset),
        1 => Just(Ans::OwnerTrue),
    ]
}

// ---- the world one case runs in ----

#[derive(Clone, Debug)]
struct PersonSpec {
    /// 0 not held, 1 member, 2 guest (what the host keeps in the person row).
    agents: [u8; 2],
    projects: [bool; 2],
}

#[derive(Clone, Debug)]
struct BotSpec {
    /// 0 the owner's, 1 and 2 the people, 3 a bot guest (see `Scenario::bot_owner`).
    owner: usize,
    deleted: bool,
    /// 0 none, 1 and 2 the agents.
    agent: usize,
}

#[derive(Clone, Debug)]
struct EntrySpec {
    /// Index into the bots; `BOTS` is a bot the store never had.
    bot: usize,
    permission: bool,
    /// 0 none, 1 its own chat, 2 another bot, 3 not a string.
    author: usize,
    /// 0 an ordinary card, 1 a key card, 2 an account card (cards/secret.rs).
    secret: usize,
    /// Index into `TOOLS`: the tool the card asks for (its title, and the tool of its card-store row).
    tool: usize,
    /// The approver stored on the entry: 0 none (an older card), 1 member, 2 owner.
    approver: usize,
    /// Its row in the card store: none (an older card), an `acp` card of a chat turn, a `run` card of a `claude -p` run.
    card: Option<CardSpec>,
}

/// A card-store row behind an entry (store/cards.rs).
#[derive(Clone, Debug)]
struct CardSpec {
    run: bool,
    /// Whose run it was: 0 and 1 the people, 2 a bot guest's person (see `Ids::card_person`), 3 nobody we know.
    person: usize,
    /// Who the host stored as its answerer when it made the card (`access::approver`): an index into `PEOPLE`, None the
    /// owner alone.
    answerer: Option<usize>,
    agent: usize,
}

/// One tool a card can ask for, labelled with what the F11 rules make of it. The labels are the rule, written here, not
/// read back from `roles`: `code` (step 1: runs code, or the host can't see into it), `outside` (step 2a: it names a file
/// outside every grant folder of the run; the test home has none, so every named file is outside), `escape` (step 2b: an
/// escape card, whose yes widens the run's next activation), `secret` (a key or account request, cards/secret.rs).
#[expect(clippy::struct_excessive_bools, reason = "four independent labels of one tool, read as the F11 rules")]
struct Tool {
    name: &'static str,
    kind: &'static str,
    input: &'static str,
    code: bool,
    outside: bool,
    escape: bool,
    secret: bool,
}

const fn tool(name: &'static str, kind: &'static str, input: &'static str, code: bool, outside: bool) -> Tool {
    Tool { name, kind, input, code, outside, escape: false, secret: false }
}

const TOOLS: [Tool; 11] = [
    tool("", "fetch", "{}", false, false),
    tool("WebFetch", "fetch", r#"{"url":"https://example.com"}"#, false, false),
    tool("Read", "read", r#"{"file_path":"/etc/hosts"}"#, false, true),
    tool("Edit", "edit", r#"{"file_path":"/Users/someone/notes.txt","old_string":"a","new_string":"b"}"#, false, true),
    tool("BashOutput", "execute", r#"{"bash_id":"1"}"#, false, false),
    tool("Bash", "execute", r#"{"command":"true"}"#, true, false),
    tool("mcp__x__y", "other", "{}", true, false),
    tool("", "other", "{}", true, false),
    tool("Grep", "execute", r#"{"pattern":"x"}"#, true, false),
    Tool {
        name: crate::cards::access::TOOL,
        kind: "other",
        input: r#"{"kind":"path","target":"read /Users/someone/Documents"}"#,
        code: false,
        outside: false,
        escape: true,
        secret: false,
    },
    Tool {
        name: crate::cards::secret::TOOL,
        kind: "other",
        input: "{}",
        code: false,
        outside: false,
        escape: false,
        secret: true,
    },
];

const BOTS: usize = 4;

#[derive(Clone, Debug)]
struct Scenario {
    /// Who owns the bot guests: 0 the Mac owner, 1 and 2 the people.
    bot_owner: usize,
    grants: [[Ans; 2]; 2],
    people: [Option<PersonSpec>; 2],
    bots: Vec<BotSpec>,
    entries: Vec<EntrySpec>,
}

fn scenario() -> impl Strategy<Value = Scenario> {
    let person = proptest::option::weighted(
        0.85,
        (proptest::array::uniform2(0..3u8), any::<[bool; 2]>())
            .prop_map(|(agents, projects)| PersonSpec { agents, projects }),
    );
    let bot = (0..4usize, proptest::bool::weighted(0.2), 0..3usize).prop_map(|(owner, deleted, agent)| BotSpec {
        owner,
        deleted,
        agent,
    });
    let card = proptest::option::weighted(
        0.6,
        (proptest::bool::weighted(0.7), 0..4usize, proptest::option::of(0..2usize), 0..2usize)
            .prop_map(|(run, person, answerer, agent)| CardSpec { run, person, answerer, agent }),
    );
    let entry = (
        0..=BOTS,
        proptest::bool::weighted(0.7),
        0..4usize,
        prop_oneof![6 => Just(0usize), 1 => 1..3usize],
        0..TOOLS.len(),
        0..3usize,
        card,
    )
        .prop_map(|(bot, permission, author, secret, tool, approver, card)| EntrySpec {
            bot,
            permission,
            author,
            secret,
            tool,
            approver,
            card,
        });
    (
        0..3usize,
        [[ans(), ans()], [ans(), ans()]],
        [person.clone(), person],
        proptest::collection::vec(bot, BOTS),
        proptest::collection::vec(entry, 0..4),
    )
        .prop_map(|(bot_owner, grants, people, bots, entries)| Scenario {
            bot_owner,
            grants,
            people,
            bots,
            entries,
        })
}

struct World {
    rt: tokio::runtime::Runtime,
    hub: Arc<Hub>,
    dir: PathBuf,
    case: Cell<u64>,
    _one_at_a_time: tokio::sync::MutexGuard<'static, ()>,
}

/// Ids of what one case put in the store.
struct Ids {
    bots: Vec<String>,
    ghost: String,
    entries: Vec<String>,
}

/// The person a card's run was for (`CardSpec::person`): the first bot guest's person when the case has one.
fn card_person(sc: &Scenario, bot_people: &[String], c: &CardSpec) -> String {
    match c.person {
        0 | 1 => PEOPLE[c.person].to_owned(),
        2 => {
            sc.bots.iter().position(|b| b.owner == 3).map_or_else(|| "bot-nobody".to_owned(), |i| bot_people[i].clone())
        }
        _ => "someone-else".to_owned(),
    }
}

impl World {
    fn new() -> Self {
        let guard = ONE_AT_A_TIME.blocking_lock();
        let dir = std::env::temp_dir().join(format!("kith-roles-props-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("grants")).unwrap();
        let bin = dir.join("kith-auth");
        // `kith-auth resolve AGENT --person PERSON`: the canned answer of that pair, failing when there is none.
        std::fs::write(&bin, "#!/bin/sh\n[ \"$1\" = resolve ] && [ \"$3\" = --person ] || exit 2\nf=\"$KITH_FAKE/$4__$2\"\n[ -f \"$f\" ] || exit 1\ncat \"$f\"\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        *TEST_ENV.lock().unwrap() = vec![("KITH_FAKE", dir.join("grants"))];
        *TEST_BIN.lock().unwrap() = Some(bin);
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let hub = rt.block_on(async {
            let store = Store::open(&dir.join("t.db")).unwrap();
            Hub::new(store, "host".into(), Identity::load_or_create(&dir).unwrap(), "token".into(), 0)
        });
        Self { rt, hub, dir, case: Cell::new(0), _one_at_a_time: guard }
    }

    fn write_grants(&self, grants: &[[Ans; 2]; 2]) {
        let d = self.dir.join("grants");
        std::fs::remove_dir_all(&d).unwrap();
        std::fs::create_dir_all(&d).unwrap();
        for (p, row) in PEOPLE.iter().zip(grants) {
            for (a, ans) in AGENTS.iter().zip(row) {
                if let Some(t) = ans.text() {
                    std::fs::write(d.join(format!("{p}__{a}")), t).unwrap();
                }
            }
        }
    }

    fn bot_config(id: &str, person: Option<&str>, agent: Option<&str>, cwd: &str) -> BotConfig {
        serde_json::from_value(json!({
            "id": id, "name": id, "backend": "claude", "cwd": cwd, "command": format!("{cwd}/run"),
            "hqAgent": agent, "person": person, "permission": "ask", "tasks": false, "connectors": [], "skills": [],
        }))
        .unwrap()
    }

    fn load(&self, sc: &Scenario) -> Ids {
        let n = self.case.get() + 1;
        self.case.set(n);
        self.write_grants(&sc.grants);
        let store = &self.hub.store;
        for (i, p) in PEOPLE.iter().enumerate() {
            store.delete_person(p).unwrap();
            if let Some(spec) = &sc.people[i] {
                store.put_person(p, p).unwrap();
                let agents = AGENTS
                    .iter()
                    .zip(spec.agents)
                    .filter(|(_, role)| *role != 0)
                    .map(|(a, role)| ((*a).to_owned(), ["", "member", "guest"][usize::from(role)].to_owned()));
                store.set_person_agents(p, &agents.collect()).unwrap();
                let projects: Vec<String> =
                    PROJECTS.iter().zip(spec.projects).filter(|(_, on)| *on).map(|(p, _)| (*p).to_owned()).collect();
                store.set_person_projects(p, &projects).unwrap();
            }
        }
        let bots: Vec<String> = (0..sc.bots.len()).map(|i| format!("c{n}b{i}")).collect();
        let bot_people: Vec<String> = (0..sc.bots.len()).map(|i| format!("bot-c{n}g{i}")).collect();
        for (i, (id, b)) in bots.iter().zip(&sc.bots).enumerate() {
            let bot_person = bot_people[i].clone();
            let person = match b.owner {
                0 => None,
                3 => Some(bot_person.as_str()),
                o => Some(PEOPLE[o - 1]),
            };
            if b.owner == 3 {
                let owner_person = (sc.bot_owner > 0).then(|| PEOPLE[sc.bot_owner - 1].to_owned());
                store.put_bot_guest(&bot_guest(&format!("c{n}bg{i}"), &bot_person, owner_person)).unwrap();
            }
            let agent = (b.agent > 0).then(|| AGENTS[b.agent - 1]);
            store.save_bot(&Self::bot_config(id, person, agent, &format!("/Users/secret-{id}"))).unwrap();
        }
        let ghost = format!("c{n}ghost");
        let mut entries = Vec::new();
        for e in &sc.entries {
            let chat = bots.get(e.bot).unwrap_or(&ghost).clone();
            let author = match e.author {
                0 => None,
                1 => Some(json!(chat)),
                2 => Some(json!(bots[(e.bot + 1) % bots.len()])),
                _ => Some(json!(7)),
            };
            let t = &TOOLS[e.tool];
            let mut data = json!({"options": [{"optionId": "allow"}], "toolKind": t.kind, "title": t.name});
            if e.approver > 0 {
                data["approver"] = json!(["", "member", "owner"][e.approver]);
            }
            if e.secret > 0 {
                data["card"] = json!(["", "key", "account"][e.secret]);
                data["secret"] = json!({"service": "github", "vars": ["GITHUB_TOKEN"], "state": "pending"});
            }
            if let Some(a) = author {
                data["author"] = a;
            }
            let kind = if e.permission { EntryKind::Permission } else { EntryKind::Notice };
            let id = store.insert_entry(&Lane::main(&chat), kind, 1, &data).unwrap().id;
            if let Some(c) = &e.card {
                let new = crate::store::NewCard {
                    session: format!("s-{id}"),
                    source: if c.run { "run" } else { "acp" },
                    bot_id: (!c.run).then(|| chat.clone()),
                    run_id: c.run.then(|| format!("r-{id}")),
                    person: card_person(sc, &bot_people, c),
                    agent: AGENTS[c.agent].to_owned(),
                    answerable_by: c.answerer.map(|p| PEOPLE[p].to_owned()),
                    in_grants: c.answerer.is_some(),
                    tool: t.name.to_owned(),
                    input: t.input.to_owned(),
                    resource: format!("k-{id}"),
                    shown: String::new(),
                    deadline: i64::MAX,
                    reply_to: None,
                    asked_by: None,
                };
                let card = match store.card_open(&new, 1).unwrap() {
                    crate::store::Opened::New(c)
                    | crate::store::Opened::Joined(c)
                    | crate::store::Opened::Granted(c) => c,
                };
                store.card_live(&card.id, Some(&id), None).unwrap();
            }
            entries.push(id);
        }
        // After the entries: deleting a bot deletes its lines.
        for (id, b) in bots.iter().zip(&sc.bots) {
            if b.deleted {
                store.delete_bot(id).unwrap();
            }
        }
        Ids { bots, ghost, entries }
    }
}

impl Drop for World {
    fn drop(&mut self) {
        *TEST_BIN.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        TEST_ENV.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
        std::fs::remove_dir_all(&self.dir).ok();
    }
}

pub(super) fn bot_guest(bot_id: &str, person_id: &str, owner_person: Option<String>) -> BotGuest {
    BotGuest {
        bot_id: bot_id.to_owned(),
        person_id: person_id.to_owned(),
        owner_person,
        name: "Rex".into(),
        provider: "grok".into(),
        agents: vec![],
        state: BotState::Active,
        device_key: None,
        cloud_device: None,
        rate_per_hour: 0,
        concurrent: 0,
        created_at: 0,
        joined_at: None,
        decided_at: None,
        decided_by: None,
        last_seen_at: None,
        revoked_at: None,
        revoked_why: None,
    }
}

fn agent_index(name: &str) -> Option<usize> {
    AGENTS.iter().position(|a| *a == name)
}

// ---- calls ----

/// Every method the router answers, read from the api sources the same way `roles_tests` does.
pub(super) fn router_methods() -> Vec<String> {
    let sources = [
        include_str!("bots.rs"),
        include_str!("calls.rs"),
        include_str!("cards.rs"),
        include_str!("host.rs"),
        include_str!("folders.rs"),
        include_str!("backups.rs"),
        include_str!("kith_info.rs"),
        include_str!("marketplace.rs"),
        include_str!("remote.rs"),
        include_str!("people.rs"),
        include_str!("botguests.rs"),
        include_str!("threads.rs"),
    ];
    let mut out = std::collections::BTreeSet::new();
    for src in sources {
        let mut open = false;
        for line in src.lines().map(str::trim) {
            let starts = line.starts_with('"') || (open && line.starts_with("| \""));
            if !starts {
                open = false;
                continue;
            }
            let head = line.split("=>").next().unwrap_or(line);
            let arm = line.contains("=>");
            if !arm && !head.trim_end().ends_with('|') {
                open = false;
                continue;
            }
            out.extend(
                head.split('"')
                    .skip(1)
                    .step_by(2)
                    .filter(|t| !t.is_empty() && t.chars().all(|c| c.is_ascii_alphanumeric()))
                    .map(str::to_owned),
            );
            open = !arm;
        }
    }
    out.extend(crate::chat::memory::HQ_METHODS.iter().map(|m| (*m).to_owned()));
    out.insert("readFile".into());
    out.into_iter().collect()
}

#[derive(Clone, Debug)]
struct CallSpec {
    method: String,
    person: usize,
    proven: bool,
    bot: usize,
    entry: usize,
    folder: usize,
    clear: usize,
    project: usize,
}

fn call_spec(routed: Vec<String>) -> impl Strategy<Value = CallSpec> {
    let method = prop_oneof![
        6 => proptest::sample::select(MEMBER_METHODS.to_vec()).prop_map(str::to_owned),
        3 => proptest::sample::select(routed),
        1 => "[a-zA-Z]{0,8}",
        1 => Just("proveInvite".to_owned()),
    ];
    (method, 0..2usize, proptest::bool::weighted(0.85), 0..8usize, 0..6usize, 0..7usize, 0..6usize, 0..8usize).prop_map(
        |(method, person, proven, bot, entry, folder, clear, project)| CallSpec {
            method,
            person,
            proven,
            bot,
            entry,
            folder,
            clear,
            project,
        },
    )
}

impl CallSpec {
    fn body(&self, ids: &Ids) -> Value {
        let mut b = serde_json::Map::new();
        match self.bot {
            0..=3 => drop(b.insert("botId".into(), json!(ids.bots[self.bot]))),
            4 => drop(b.insert("botId".into(), json!(ids.ghost))),
            5 => drop(b.insert("botId".into(), json!(""))),
            6 => {}
            _ => drop(b.insert("botId".into(), json!(7))),
        }
        match self.entry {
            i if i < ids.entries.len() => drop(b.insert("entryId".into(), json!(ids.entries[i]))),
            3 => drop(b.insert("entryId".into(), json!("no-such-entry"))),
            4 => drop(b.insert("entryId".into(), json!(""))),
            _ => {}
        }
        match self.folder {
            0 => {}
            1 => drop(b.insert("folder".into(), json!(""))),
            2 => drop(b.insert("folder".into(), json!("   "))),
            3 => drop(b.insert("folder".into(), json!("/x"))),
            4 => drop(b.insert("folder".into(), Value::Null)),
            5 => drop(b.insert("folder".into(), json!(7))),
            _ => drop(b.insert("folder".into(), json!(" /x "))),
        }
        match self.clear {
            0 => {}
            1 => drop(b.insert("clear".into(), json!(true))),
            2 => drop(b.insert("clear".into(), json!(false))),
            3 => drop(b.insert("clear".into(), json!("true"))),
            4 => drop(b.insert("clear".into(), json!(1))),
            _ => drop(b.insert("clear".into(), Value::Null)),
        }
        let project = [
            None,
            Some(json!("")),
            Some(json!("  ")),
            Some(json!("pr1")),
            Some(json!("pr2")),
            Some(json!(" pr1 ")),
            Some(json!("other")),
            Some(json!(7)),
        ];
        if let Some(p) = &project[self.project] {
            b.insert("project".into(), p.clone());
        }
        Value::Object(b)
    }
}

/// Method names that name their chat through an entry, not a `botId`.
fn by_entry(m: &str) -> bool {
    matches!(m, "respondPermission" | "react" | "editHandoff" | "keepConversation")
}

fn nothing_to_check(m: &str) -> bool {
    matches!(
        m,
        "hello"
            | "projects"
            | "linkCode"
            | "scheduleMessages"
            | "startChat"
            | "callAccept"
            | "callDecline"
            | "callGreet"
            | "resolveCitation"
            | "createBotInvite"
            | "revokeBotInvite"
            | "revokeBot"
            | "myBots"
            | "setBotLimits"
    )
}

fn fail(msg: impl Into<String>) -> TestCaseError {
    TestCaseError::fail(msg.into())
}

/// Everything a successful `guard` from `person` must imply, stated from the scenario and not from `roles`.
fn check_guard_ok(world: &World, sc: &Scenario, call: &CallSpec, ids: &Ids, body: &Value) -> Result<(), TestCaseError> {
    let me = PEOPLE[call.person];
    let method = call.method.as_str();
    if !MEMBER_METHODS.contains(&method) {
        return Err(fail(format!("{method} is not on MEMBER_METHODS but guard let {me} through")));
    }
    if nothing_to_check(method) {
        return Ok(());
    }
    // The chat the call is about.
    let chat = if by_entry(method) {
        let entry = world.hub.store.entry(body["entryId"].as_str().unwrap_or_default());
        let Some(entry) = entry else { return Err(fail("guard let an unknown entry through")) };
        let spec = &sc.entries[call.entry];
        let t = &TOOLS[spec.tool];
        if roles::secret_card(&entry.data) {
            return Err(fail(format!("{method} let a member through for a key or account card")));
        }
        if spec.card.is_some() && t.secret {
            return Err(fail(format!(
                "{method} let a member through for a card whose card-store tool is a key request"
            )));
        }
        if method == "respondPermission" {
            if entry.kind != "permission" {
                return Err(fail("respondPermission let through for a card that is not a permission request"));
            }
            // (21) Who answers is stored on the card when it opens (`access::approver`), wherever it shows: the person
            // it names, while kith-auth says they are a member of its agent now (A7). What the card names is the
            // producer's to judge (code, files outside the grants, escape cards: cards/tests.rs), not the guard's.
            let Some(c) = &spec.card else {
                return Err(fail("respondPermission let through a card with no card-store row"));
            };
            if c.answerer != Some(call.person) {
                return Err(fail(format!("respondPermission let {me} answer a card naming {:?}", c.answerer)));
            }
            if !sc.grants[call.person][c.agent].member() {
                return Err(fail(format!(
                    "{me} answered a card of {} without being a member of it now",
                    AGENTS[c.agent]
                )));
            }
            return Ok(());
        }
        entry.bot_id
    } else {
        body["botId"].as_str().unwrap_or_default().to_owned()
    };
    let row =
        world.hub.store.bot(&chat).unwrap().ok_or_else(|| fail("guard let through a chat that does not exist"))?;
    if row.deleted {
        return Err(fail("guard let through a deleted chat"));
    }
    if row.config.person.as_deref() != Some(me) {
        // The one way into a chat that is not theirs (a card naming them aside, checked above):
        let theirs = row
            .config
            .person
            .as_deref()
            .and_then(|bot| world.hub.store.bot_guest_of_person(bot))
            .is_some_and(|g| g.owner_person.as_deref() == Some(me));
        // ...reading the chat of a bot guest of theirs, which no write method reaches.
        if theirs && ["history", "thread", "kithInfo", "threadProjects"].contains(&method) {
            return Ok(());
        }
        return Err(fail(format!("guard let {me} into a chat of {:?} ({method})", row.config.person)));
    }
    let Some(agent) = row.config.hq_agent.as_deref().and_then(agent_index) else {
        return Err(fail("guard let through a chat with no (known) agent"));
    };
    let ans = &sc.grants[call.person][agent];
    if !ans.allowed() {
        return Err(fail(format!("kith-auth says {ans:?} for {me}/{} and guard let through", AGENTS[agent])));
    }
    if method == "send" {
        match Change::from_args(body) {
            Ok(Change::Nothing) => {}
            Ok(Change::Set(Pick::Project(project))) => {
                let shared = sc.people[call.person]
                    .as_ref()
                    .is_some_and(|s| PROJECTS.iter().zip(s.projects).any(|(x, on)| on && *x == project));
                if !shared {
                    return Err(fail(format!("send let {me} start in project {project} that is not shared with them")));
                }
            }
            other => return Err(fail(format!("send let through a folder pick: {body} -> {other:?}"))),
        }
    }
    let _ = ids;
    Ok(())
}

/// A read of the chat of a bot guest the person owns: held by that ownership, not by the agent grant (the chat of an
/// ended bot stays readable to its owner).
fn reads_bot_guest_chat(w: &World, me: &str, method: &str, b: &Value) -> bool {
    ["history", "thread", "kithInfo", "threadProjects"].contains(&method)
        && b["botId"]
            .as_str()
            .and_then(|c| w.hub.store.bot(c).ok().flatten())
            .and_then(|r| r.config.person)
            .and_then(|p| w.hub.store.bot_guest_of_person(&p))
            .is_some_and(|g| g.owner_person.as_deref() == Some(me))
}

#[test]
fn guard_never_lets_a_person_past_their_grant() {
    let w = World::new();
    let routed = router_methods();
    assert!(routed.len() > 100, "router walk found {} methods", routed.len());
    runner()
        .run(&(scenario(), call_spec(routed)), |(sc, c)| {
            let ids = w.load(&sc);
            let b = c.body(&ids);
            let me = PEOPLE[c.person];
            let verdict = w.rt.block_on(roles::guard(&w.hub, me, c.proven, &c.method, &b));
            if !c.proven {
                // (2) A device that has not shown its invite may only show it.
                prop_assert_eq!(
                    verdict.is_ok(),
                    UNPROVEN_METHODS.contains(&c.method.as_str()),
                    "unproven {}",
                    c.method
                );
                if let Err(e) = &verdict {
                    prop_assert!(e.is::<NotAllowed>());
                }
                return Ok(());
            }
            // (2) A proven device that shows its invite again is told it already has, in the words the apps wait for.
            if c.method == "proveInvite" {
                let e = verdict.as_ref().err().map(|e| format!("{e:#}")).unwrap_or_default();
                prop_assert!(e.contains("already shown its invite"), "proven proveInvite: {e:?}");
                return Ok(());
            }
            match verdict {
                Ok(()) => {
                    check_guard_ok(&w, &sc, &c, &ids, &b)?;
                    // The same call after the grant is taken away must be refused (kith-auth is asked every call).
                    if !nothing_to_check(&c.method) && !reads_bot_guest_chat(&w, me, &c.method, &b) {
                        let mut gone = sc.clone();
                        gone.grants[c.person] = [Ans::Denied(false), Ans::Denied(false)];
                        w.write_grants(&gone.grants);
                        let again = w.rt.block_on(roles::guard(&w.hub, me, true, &c.method, &b));
                        prop_assert!(again.is_err(), "{} still passes after the grant was removed", c.method);
                    }
                }
                Err(e) => {
                    if !MEMBER_METHODS.contains(&c.method.as_str()) {
                        prop_assert!(e.is::<NotAllowed>(), "{}: {e:#}", c.method);
                    }
                }
            }
            Ok(())
        })
        .unwrap();
}

/// A plain card (no tool named, kind fetch, no approver stored, no card-store row) for the hand-written cases.
fn entry(bot: usize, permission: bool, author: usize, secret: usize) -> EntrySpec {
    EntrySpec { bot, permission, author, secret, tool: 0, approver: 0, card: None }
}

/// Soundness alone passes a guard that refuses everything: these calls must get through.
#[test]
fn guard_lets_a_granted_member_use_their_own_chat() {
    let w = World::new();
    let card = |run: bool, person: usize, answerer: Option<usize>, agent: usize| {
        Some(CardSpec { run, person, answerer, agent })
    };
    let run = |person: usize, answerer: Option<usize>, tool: usize| EntrySpec {
        tool,
        card: card(true, person, answerer, 0),
        ..entry(0, true, 1, 0)
    };
    let sc = Scenario {
        bot_owner: 1,
        grants: [[Ans::Member, Ans::Guest], [Ans::Denied(false), Ans::Denied(false)]],
        people: [Some(PersonSpec { agents: [1, 1], projects: [true, false] }), None],
        bots: vec![
            BotSpec { owner: 1, deleted: false, agent: 1 },
            BotSpec { owner: 1, deleted: false, agent: 2 },
            BotSpec { owner: 2, deleted: false, agent: 1 },
            BotSpec { owner: 0, deleted: false, agent: 1 },
            BotSpec { owner: 1, deleted: true, agent: 1 },
            BotSpec { owner: 3, deleted: false, agent: 1 },
        ],
        entries: vec![
            EntrySpec { card: card(false, 0, Some(0), 0), ..entry(0, true, 1, 0) },
            entry(1, true, 0, 0),
            entry(0, false, 1, 0),
            entry(0, true, 2, 0),
            entry(0, true, 1, 1),
            entry(0, true, 0, 2),
            // 6..: cards of `claude -p` runs, by who the host stored as their answerer.
            run(0, Some(0), 1),
            run(0, None, 1),
            run(1, Some(1), 1),
            run(2, Some(0), 1),
            EntrySpec { card: card(true, 0, Some(0), 1), ..entry(0, true, 1, 0) },
            // 11..: chat cards.
            EntrySpec { tool: 5, ..entry(0, true, 1, 0) },
            EntrySpec { tool: 1, approver: 2, ..entry(0, true, 1, 0) },
            EntrySpec { tool: 10, card: card(false, 0, Some(0), 0), ..entry(0, true, 1, 0) },
            EntrySpec { tool: 1, approver: 1, card: card(false, 0, Some(0), 0), ..entry(0, true, 1, 0) },
            EntrySpec { tool: 1, approver: 1, ..entry(0, true, 1, 0) },
        ],
    };
    let ids = w.load(&sc);
    let go = |m: &str, b: Value| w.rt.block_on(roles::guard(&w.hub, "p1", true, m, &b));
    assert!(go("send", json!({"botId": ids.bots[0], "project": "pr1"})).is_ok());
    assert!(go("history", json!({"botId": ids.bots[1]})).is_ok());
    assert!(go("respondPermission", json!({"entryId": ids.entries[0]})).is_ok(), "a member answers their own prompt");
    assert!(go("respondPermission", json!({"entryId": ids.entries[1]})).is_err(), "a guest's prompt is the owner's");
    assert!(go("send", json!({"botId": ids.bots[2]})).is_err(), "another person's chat");
    assert!(go("send", json!({"botId": ids.bots[3]})).is_err(), "the owner's chat");
    assert!(go("send", json!({"botId": ids.bots[0], "project": "pr2"})).is_err());
    assert!(go("send", json!({"botId": ids.bots[0], "folder": "/x"})).is_err());
    assert!(go("send", json!({"botId": ids.bots[0], "clear": true})).is_err());
    // Not a prompt of their run: a card of another kind, and a card another bot raised.
    assert!(go("respondPermission", json!({"entryId": ids.entries[2]})).is_err(), "a notice is not a prompt");
    assert!(go("respondPermission", json!({"entryId": ids.entries[3]})).is_err(), "another bot's prompt");
    // A key or account card, even one that looks like the member's own prompt, is the owner's.
    for i in [4, 5] {
        for m in ["respondPermission", "react", "editHandoff", "keepConversation"] {
            assert!(go(m, json!({"entryId": ids.entries[i]})).is_err(), "{m} on a secret card");
        }
    }
    assert!(go("send", json!({"botId": ids.bots[4]})).is_err(), "a deleted chat");
    let answer = |i: usize| go("respondPermission", json!({"entryId": ids.entries[i]}));
    assert!(answer(6).is_ok(), "a member answers a run card naming them");
    assert!(answer(7).is_err(), "a run card stored with the owner alone");
    assert!(answer(8).is_err(), "a run card naming someone else");
    assert!(answer(9).is_ok(), "the bot guest's owner answers the card naming them (7.6)");
    assert!(answer(10).is_err(), "a card naming p1 for an agent they are no longer a member of");
    assert!(answer(11).is_err(), "a chat card with no card-store row is the owner's");
    assert!(answer(12).is_err(), "a chat card stored with the owner as approver");
    for m in ["respondPermission", "react", "editHandoff", "keepConversation"] {
        assert!(
            go(m, json!({"entryId": ids.entries[13]})).is_err(),
            "{m} on a card whose card-store tool asks for a key"
        );
    }
    assert!(answer(14).is_ok(), "a chat card naming p1 in its card-store row");
    assert!(answer(15).is_err(), "an older card that says member but has no card-store row");
    // Every method off the member list is refused, one by one (a random draw hits a single method too rarely).
    for m in router_methods().iter().filter(|m| !MEMBER_METHODS.contains(&m.as_str())) {
        let body = json!({"botId": ids.bots[0], "entryId": ids.entries[0]});
        assert!(go(m, body).is_err(), "{m} is not on MEMBER_METHODS but guard let a member through");
    }
    // The grant taken away: the same calls are refused the next time.
    w.write_grants(&[[Ans::Denied(false), Ans::Denied(false)], [Ans::Denied(false), Ans::Denied(false)]]);
    assert!(go("send", json!({"botId": ids.bots[0]})).is_err(), "a revoked grant still sends");
    assert!(go("history", json!({"botId": ids.bots[1]})).is_err(), "a revoked grant still reads");
    assert!(answer(6).is_err(), "a revoked member still answers the card naming them");
}

/// (21) What a card names is judged once, when it opens (`access::tool_in_grants`): a person can answer it only when it
/// runs no code and names no file outside the run's grant folders (F11 steps 1 and 2a). Escape and key cards never ask.
#[test]
fn a_card_is_within_a_persons_grants_only_when_the_f11_rules_say_so() {
    let _w = World::new();
    for t in TOOLS.iter().filter(|t| !t.escape && !t.secret) {
        let input: Value = serde_json::from_str(t.input).unwrap();
        assert_eq!(
            crate::access::tool_in_grants("p1", "a1", t.name, t.kind, &input),
            !t.code && !t.outside,
            "{} ({})",
            t.name,
            t.kind
        );
    }
}

/// (7) kith-auth's answer, every kind: only a grant counts, anything unreadable is a no, a member alone approves.
#[test]
fn kith_auth_answers_fail_closed() {
    let w = World::new();
    let mut all =
        vec![Ans::Fail, Ans::Denied(false), Ans::Denied(true), Ans::Member, Ans::Guest, Ans::NoPreset, Ans::OwnerTrue];
    all.extend((0..GARBAGE.len()).map(Ans::Garbage));
    all.extend((0..OTHER_PRESETS.len()).map(Ans::Other));
    for a in all {
        w.write_grants(&[[a.clone(), Ans::Fail], [Ans::Fail, Ans::Fail]]);
        let ask = |in_grants| crate::access::Request {
            person: "p1",
            agent: "a1",
            owner_run: false,
            ask: crate::access::Ask::Tool { in_grants },
        };
        let (may, checked, approves, outside) = w.rt.block_on(async {
            (
                roles::may_use("p1", "a1").await,
                roles::may_use_checked("p1", "a1").await,
                crate::access::approver(&w.hub.store, &ask(true)).await,
                crate::access::approver(&w.hub.store, &ask(false)).await,
            )
        });
        assert_eq!(may, a.allowed(), "may_use for {a:?}");
        // The checked form says yes exactly when may_use does, and "couldn't ask" (never yes) when kith-auth gave no
        // answer or named the owner: a caller that retries on None must not take it as a grant.
        assert_eq!(checked == Some(true), a.allowed(), "may_use_checked for {a:?}: {checked:?}");
        if matches!(a, Ans::Fail | Ans::OwnerTrue) {
            assert_eq!(checked, None, "may_use_checked for {a:?}");
        }
        assert_eq!(approves.is_some(), a.member(), "approver for {a:?}");
        assert_eq!(outside, None, "a member answered what is not within their grants for {a:?}");
        // An agent or person the table doesn't list is a failure of kith-auth: denied.
        assert!(!w.rt.block_on(roles::may_use("p1", "a2")));
        assert!(!w.rt.block_on(roles::may_use("nobody", "a1")));
        // And the agents list never offers more than kith-auth allows, nor calls a non-member a member.
        let case = Scenario {
            bot_owner: 0,
            grants: [[a.clone(), Ans::Fail], [Ans::Fail, Ans::Fail]],
            people: [None, None],
            bots: vec![BotSpec { owner: 0, deleted: false, agent: 1 }, BotSpec { owner: 0, deleted: false, agent: 2 }],
            entries: vec![],
        };
        w.load(&case);
        let listed = w.rt.block_on(roles::granted_agents(&w.hub, "p1"));
        assert!(
            listed.iter().all(|r| r["agent"] == "a1" && a.allowed() && ((r["role"] == "member") == a.member())),
            "{a:?}: {listed:?}"
        );
        assert_eq!(listed.len(), usize::from(a.allowed()), "{a:?}");
    }
}

// ---- events ----

#[derive(Clone, Debug)]
struct EventSpec {
    kind: usize,
    bot: usize,
    key: usize,
    entry_kind: usize,
    who: usize,
    /// 0 an ordinary entry, 1 a key card, 2 an account card.
    secret: usize,
}

const EVENT_TYPES: [&str; 10] =
    ["bot", "entry", "usage", "screens", "cloud", "resync", "terminal", "", "zzz", "botUpdated"];

fn event_spec() -> impl Strategy<Value = EventSpec> {
    (0..EVENT_TYPES.len(), 0..=BOTS, 0..3usize, 0..2usize, 0..3usize, 0..3usize)
        .prop_map(|(kind, bot, key, entry_kind, who, secret)| EventSpec { kind, bot, key, entry_kind, who, secret })
}

/// Everything in an event about a chat that a person must not see, tagged so a leak shows up in the text.
const SECRET: &str = "SECRET-PATH";

fn event_value(e: &EventSpec, ids: &Ids, hub: &Hub) -> Value {
    let chat = ids.bots.get(e.bot).unwrap_or(&ids.ghost).clone();
    let kind = if e.entry_kind == 0 { "permission" } else { "notice" };
    match EVENT_TYPES[e.kind] {
        "bot" => {
            let mut bot = match hub.store.bot(&chat).unwrap() {
                Some(row) => hub.bot_json(&row),
                None => json!({"id": chat}),
            };
            bot["id"] = json!(chat);
            for k in ["cwd", "command", "folder"] {
                bot[k] = json!(SECRET);
            }
            json!({"type": "bot", "bot": bot})
        }
        "entry" => {
            let mut entry = json!({"kind": kind, "text": "hi"});
            if e.secret > 0 {
                let card = ["", "key", "account"][e.secret];
                entry["data"] = json!({"card": card, "secret": {"service": "github"}});
            }
            match e.key {
                0 => entry["botId"] = json!(chat),
                1 => entry["bot_id"] = json!(chat),
                _ => {
                    entry["botId"] = json!(7);
                    entry["bot_id"] = json!(chat);
                }
            }
            json!({"type": "entry", "entry": entry})
        }
        "botUpdated" => {
            let owner = [json!("p1"), json!("p2"), json!(7)][e.key].clone();
            json!({"type": "botUpdated", "bot": {"id": chat, "ownerPerson": owner}})
        }
        t => json!({"type": t, "rev": 1}),
    }
}

#[test]
fn events_reach_only_the_person_they_are_about() {
    let w = World::new();
    runner()
        .run(&(scenario(), event_spec()), |(sc, e)| {
            let ids = w.load(&sc);
            let ev = event_value(&e, &ids, &w.hub);
            let who = [None, Some("p1"), Some("p2")][e.who];
            let out = roles::event_for(&w.hub.store, who, ev.clone());
            // A key or account card goes to the owner's devices only: no person gets it, whatever chat it names.
            if who.is_some() && e.secret > 0 && EVENT_TYPES[e.kind] == "entry" {
                prop_assert!(out.is_none(), "a secret card reached a person: {out:?}");
            }
            let ty = EVENT_TYPES[e.kind];
            let chat = ids.bots.get(e.bot).unwrap_or(&ids.ghost);
            // The chat an entry event is about is `botId`, else `bot_id` (what the code reads; a non-string `botId` falls through).
            let about = matches!(ty, "bot" | "entry").then_some(chat.as_str());
            let row = about.and_then(|c| w.hub.store.bot(c).unwrap());
            match (who, out) {
                (Some(p), Some(v)) => {
                    // (5) A person's event is about their own chat, of an agent the host still lists for them.
                    prop_assert!(["bot", "entry", "resync", "botUpdated"].contains(&ty), "{ty} reached {p}");
                    if ty == "botUpdated" {
                        prop_assert_eq!(v["bot"]["ownerPerson"].as_str(), Some(p), "a bot's update reached a stranger");
                    } else if ty != "resync" {
                        let row = row.ok_or_else(|| fail("event about an unknown chat reached a person"))?;
                        let i = PEOPLE.iter().position(|x| *x == p).unwrap();
                        let role = |a: usize| sc.people[i].as_ref().map_or(0, |s| s.agents[a]);
                        let agent = row.config.hq_agent.as_deref().and_then(agent_index);
                        if row.config.person.as_deref() == Some(p) {
                            // Their own chat, of an agent the host still lists for them.
                            let listed = agent.is_some_and(|a| role(a) != 0);
                            prop_assert!(listed, "event about a chat whose agent is not granted reached {p}");
                        } else {
                            // Not theirs: only the chat of a bot guest of theirs, whole (read-only for them).
                            let theirs = row
                                .config
                                .person
                                .as_deref()
                                .and_then(|b| w.hub.store.bot_guest_of_person(b))
                                .is_some_and(|g| g.owner_person.as_deref() == Some(p));
                            prop_assert!(theirs, "an event about {:?}'s chat reached {p}", row.config.person);
                        }
                        // (6) and nothing of where the agent works.
                        prop_assert!(!v.to_string().contains(SECRET), "{v}");
                        prop_assert!(!v.to_string().contains("/Users/secret-"), "{v}");
                    }
                }
                (Some(_), None) => {}
                (None, Some(v)) => {
                    // The owner sees an invitee's chat only as a permission prompt; a bot guest's chat whole.
                    if let Some(row) = &row
                        && row.config.person.is_some()
                        && row.config.person.as_deref().and_then(|b| w.hub.store.bot_guest_of_person(b)).is_none()
                    {
                        prop_assert!(
                            ty == "entry" && v["entry"]["kind"] == "permission",
                            "{ty} about an invitee's chat reached the owner"
                        );
                    }
                }
                (None, None) => {
                    if let Some(row) = &row {
                        prop_assert!(row.config.person.is_some(), "an owner event was dropped for the owner");
                    }
                }
            }
            Ok(())
        })
        .unwrap();
}

// ---- shaping ----

fn junk() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![Just(Value::Null), any::<bool>().prop_map(Value::from), "[a-z/]{0,6}".prop_map(Value::from)];
    leaf.prop_recursive(2, 8, 3, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..3).prop_map(Value::Array),
            proptest::collection::hash_map("[a-z]{1,4}|cwd|command|folder|path|error|options", inner, 0..4)
                .prop_map(|m| Value::Object(m.into_iter().collect())),
        ]
    })
}

fn with_secrets(v: Value, keys: &[&str]) -> Value {
    let mut o = match v {
        Value::Object(o) => o,
        _ => serde_json::Map::new(),
    };
    for k in keys {
        o.insert((*k).to_owned(), json!(SECRET));
    }
    Value::Object(o)
}

#[test]
fn what_a_person_is_shown_has_no_paths_folders_or_error_text() {
    let w = World::new();
    let sc = Scenario {
        bot_owner: 0,
        grants: [[Ans::Fail, Ans::Fail], [Ans::Fail, Ans::Fail]],
        people: [Some(PersonSpec { agents: [0, 0], projects: [true, false] }), None],
        bots: vec![],
        entries: vec![],
    };
    w.load(&sc);
    let listed = |v: &Value| v.as_array().cloned().unwrap_or_default();
    runner()
        .run(&(junk(), junk(), proptest::collection::vec((junk(), 0..3usize), 0..4)), |(base, bot, projects)| {
            // hello: only the allowed names survive.
            let hello =
                roles::shape(&w.hub, "p1", "hello", with_secrets(base.clone(), &["cwd", "addr", "screens", "path"]));
            let keep = [
                "hostId",
                "computerId",
                "signKey",
                "boxKey",
                "name",
                "version",
                "protocol",
                "minProtocol",
                "os",
                "rev",
                "store",
            ];
            prop_assert!(hello.as_object().is_none_or(|o| o.keys().all(|k| keep.contains(&k.as_str()))), "{hello}");
            // history / thread: not the folder.
            for m in ["history", "thread"] {
                let v = roles::shape(&w.hub, "p1", m, with_secrets(base.clone(), &["folder"]));
                prop_assert!(v.get("folder").is_none(), "{m}: {v}");
            }
            // projects: shared ones only, an id and a name, no error text.
            let list: Vec<Value> = projects
                .iter()
                .map(|(extra, i)| {
                    let mut p = with_secrets(extra.clone(), &["path", "folders", "cwd"]);
                    p["id"] = json!(["pr1", "pr2", "pr3"][*i]);
                    p
                })
                .collect();
            let v = roles::shape(&w.hub, "p1", "projects", json!({"projects": list, "error": SECRET}));
            prop_assert!(v.get("error").is_none());
            for p in listed(&v["projects"]) {
                prop_assert_eq!(p["id"].as_str(), Some("pr1"));
                prop_assert_eq!(p.as_object().map(serde_json::Map::len), Some(2));
                prop_assert!(!p.to_string().contains(SECRET));
            }
            // kithInfo: a name, description and avatar and when it was fetched.
            let info = roles::shape(
                &w.hub,
                "p1",
                "kithInfo",
                json!({"info": with_secrets(base.clone(), &["cwd", "name", "path"]), "fetchedAt": 1, "path": SECRET}),
            );
            prop_assert!(info.as_object().unwrap().keys().all(|k| k == "info" || k == "fetchedAt"), "{info}");
            prop_assert!(
                info["info"]
                    .as_object()
                    .is_none_or(|o| o.keys().all(|k| ["name", "description", "avatar"].contains(&k.as_str())))
            );
            // shape_bot and hello_for.
            let b = roles::shape_bot(json!({"type": "bot", "bot": with_secrets(bot, &["cwd", "command", "folder"])}));
            for k in ["cwd", "command", "folder"] {
                prop_assert!(b["bot"].get(k).is_none(), "{k} left in {b}");
            }
            let h = roles::hello_for(&with_secrets(base, &["cwd", "path", "signKey"]));
            prop_assert!(
                h.as_object().unwrap().keys().all(|k| ["type", "hostId", "computerId", "rev"].contains(&k.as_str())),
                "{h}"
            );
            Ok(())
        })
        .unwrap();
}

/// The bot as the host really sends it (`bot_json`) shows no place on this computer once shaped.
#[test]
fn a_real_bot_json_loses_where_it_works() {
    let w = World::new();
    let sc = Scenario {
        bot_owner: 0,
        grants: [[Ans::Member, Ans::Fail], [Ans::Fail, Ans::Fail]],
        people: [Some(PersonSpec { agents: [1, 0], projects: [false, false] }), None],
        bots: vec![BotSpec { owner: 1, deleted: false, agent: 1 }, BotSpec { owner: 1, deleted: false, agent: 2 }],
        entries: vec![],
    };
    let ids = w.load(&sc);
    // Their chat of an agent their person row no longer lists does not reach them.
    let gone = json!({"type": "bot", "bot": {"id": ids.bots[1]}});
    assert!(roles::event_for(&w.hub.store, Some("p1"), gone).is_none(), "a chat of an agent they lost");
    let row = w.hub.store.bot(&ids.bots[0]).unwrap().unwrap();
    let ev = json!({"type": "bot", "bot": w.hub.bot_json(&row)});
    assert!(ev.to_string().contains("/Users/secret-"), "the fixture must carry a path or this checks nothing");
    // The owner of the computer hears an invitee's chat only as a permission prompt.
    assert!(roles::event_for(&w.hub.store, None, ev.clone()).is_none(), "an invitee's chat reached the owner");
    let say = json!({"type": "entry", "entry": {"botId": ids.bots[0], "kind": "agent"}});
    assert!(roles::event_for(&w.hub.store, None, say).is_none(), "an invitee's words reached the owner");
    let seen = roles::event_for(&w.hub.store, Some("p1"), ev).expect("their own chat reaches them");
    assert!(!seen.to_string().contains("/Users/secret-"), "{seen}");
}

// ---- bot guests ----

/// A bot guest gets a short list and nothing a member's chat, file or project call would give it.
#[test]
fn a_bot_guest_may_call_only_its_short_list() {
    let routed = router_methods();
    for m in roles::BOT_METHODS {
        assert!(routed.iter().any(|r| r == m), "{m} is on BOT_METHODS but the router has no such method");
        assert!(*m == "hello" || !roles::member_may(m), "{m} is on both lists: a bot would hold a member's call");
    }
    for m in roles::BOT_PENDING_METHODS {
        assert!(roles::BOT_METHODS.contains(m), "{m}: a bot that is not allowed in yet holds more than an allowed one");
    }
    for m in [
        "respondPermission",
        "startChat",
        "send",
        "thread",
        "history",
        "upload",
        "readUpload",
        "projects",
        "linkCode",
        "createBotInvite",
        "revokeBot",
        "myBots",
        "setBotLimits",
        "provideSecret",
        "listKeys",
        "revokeKey",
        "createBot",
        "updateBot",
        "deleteBot",
        "createInvite",
        "people",
    ] {
        assert!(!roles::bot_may(m), "{m} must not be a bot's");
    }
    runner()
        .run(&proptest::sample::select(routed), |m| {
            prop_assert_eq!(roles::bot_may(&m), roles::BOT_METHODS.contains(&m.as_str()));
            Ok(())
        })
        .unwrap();
    runner()
        .run(&"[a-zA-Z]{0,10}", |m| {
            prop_assert_eq!(roles::bot_may(&m), roles::BOT_METHODS.contains(&m.as_str()));
            Ok(())
        })
        .unwrap();
}

fn entry_json() -> impl Strategy<Value = Value> {
    let kind = proptest::sample::select(vec!["user", "agent", "thought", "tool", "plan", "permission", "notice", "x"]);
    let data = (
        "[a-z ]{0,8}",
        proptest::sample::select(vec!["pending", "done", "error", "info"]),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(|(text, state, sender, extra)| {
            let mut d = json!({
                "text": text, "status": state, "style": state, "final": extra,
                // What a bot must never see, under every key an entry may carry it in.
                "tool": SECRET, "args": SECRET, "title": SECRET, "attachments": [SECRET], "options": [SECRET],
                "input": {"command": SECRET}, "path": SECRET, "cwd": SECRET, "author": SECRET,
            });
            if sender {
                d["sender"] = json!({"botId": "b", "name": "Rex", "speaker": "bot", "path": SECRET, "cwd": SECRET});
            }
            d
        });
    (kind, data).prop_map(|(kind, data)| {
        json!({"id": "e1", "seq": 3, "turn": 1, "createdAt": 5, "kind": kind, "data": data, "botId": SECRET, "threadId": SECRET})
    })
}

/// What a bot guest is shown of a transcript: words and a bare "waiting", never a tool, its arguments, a path or a card.
#[test]
fn a_bot_guest_is_shown_words_only() {
    runner()
        .run(&entry_json(), |e| {
            let out = roles::bot_entry(&e, "conv1", "Sam");
            let kind = e["kind"].as_str().unwrap_or_default();
            let Some(out) = out else {
                prop_assert!(!matches!(kind, "user" | "agent"), "{kind} entry dropped");
                return Ok(());
            };
            prop_assert!(!out.to_string().contains(SECRET), "{out}");
            prop_assert!(
                out.as_object().unwrap().keys().all(|k| {
                    ["id", "seq", "turn", "at", "conversation", "kind", "text", "sender", "final", "error"]
                        .contains(&k.as_str())
                }),
                "{out}"
            );
            prop_assert!(["user", "agent", "notice", "waiting"].contains(&out["kind"].as_str().unwrap()), "{out}");
            if out["kind"] == "waiting" {
                prop_assert_eq!(out["text"].as_str(), Some("Waiting for Sam to approve a step"));
                prop_assert!(kind == "permission" && e["data"]["status"] == "pending");
            }
            if out["kind"] == "notice" {
                prop_assert!(kind == "notice" && e["data"]["style"] == "error");
            }
            if let Some(sender) = out.get("sender") {
                prop_assert!(
                    sender.as_object().unwrap().keys().all(|k| ["botId", "name", "speaker"].contains(&k.as_str()))
                );
            }
            Ok(())
        })
        .unwrap();
}

#[test]
fn the_store_health_reaches_a_member_without_its_error_text_and_only_the_owner_hears_the_event() {
    let w = World::new();
    let hello = json!({"hostId": "h", "store": {"writable": false, "unsaved": 2, "dropped": 0, "error": SECRET}});
    let shaped = roles::shape(&w.hub, "p1", "hello", hello);
    assert_eq!(shaped["store"]["writable"], false);
    assert!(shaped["store"].get("error").is_none(), "{shaped}");
    let event = json!({"type": "storeHealth", "store": w.hub.store_health(false)});
    assert!(roles::event_for(&w.hub.store, Some("p1"), event.clone()).is_none(), "a member hears no host event");
    assert!(roles::event_for(&w.hub.store, None, event).is_some());
    assert!(w.hub.store_health(false).get("error").is_none(), "the event never carries the error text");
}

/// Keys are the owner's alone (cards/secret.rs): on no list a guest, member, bot or waiting bot can call from, by name,
/// in addition to the router walk above.
#[test]
fn the_key_methods_are_on_no_guest_list() {
    let routed = router_methods();
    for m in ["provideSecret", "listKeys", "revokeKey"] {
        assert!(routed.iter().any(|r| r == m), "{m} is not found by the router walk");
        assert!(!MEMBER_METHODS.contains(&m), "{m} on MEMBER_METHODS");
        assert!(!roles::BOT_METHODS.contains(&m), "{m} on BOT_METHODS");
        assert!(!roles::BOT_PENDING_METHODS.contains(&m), "{m} on BOT_PENDING_METHODS");
        assert!(!UNPROVEN_METHODS.contains(&m), "{m} on UNPROVEN_METHODS");
        assert!(!roles::member_may(m) && !roles::bot_may(m));
    }
}
