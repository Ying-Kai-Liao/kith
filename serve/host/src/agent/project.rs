//! The project of a chat, decided per turn from what is being discussed (docs/memory-context.md, "Project inference per
//! turn, and the divider").
//!
//! Cheap and deterministic: the user's message (and, once, the last couple of their messages) is searched for the ids,
//! names and folders of the projects hq knows; no model is called. A thread keeps its project set unless the subject
//! clearly moved, and moves at most once in a few messages, so a stray mention doesn't flap it. A person's correction
//! is the same kind of state with `by: "person"`, and the same rule moves it on. `BotConfig.project` is an explicit pin
//! and is never inferred over (the caller leaves this module out of it).

use crate::agent::hq::Project;
use crate::store::{Lane, Store};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// A thread's current project set moves again (by inference) only after this many messages of staying.
const COOLDOWN: u32 = 3;

/// Folder names, and single-word ids, too ordinary to say anything about a project.
const GENERIC: &[&str] = &[
    "src",
    "code",
    "work",
    "home",
    "main",
    "test",
    "tests",
    "docs",
    "app",
    "apps",
    "web",
    "site",
    "notes",
    "misc",
    "temp",
    "tmp",
    "data",
    "documents",
    "desktop",
    "projects",
    "project",
    "repos",
    "github",
    "workspace",
    "dev",
    "build",
    "dist",
    "lib",
    "bin",
    "public",
    "private",
    "files",
    "downloads",
    "users",
    "play",
    "life",
    "personal",
    "blog",
    "game",
    "games",
    "shop",
    "store",
    "tools",
    "tool",
    "demo",
    "demos",
    "list",
    "todo",
    "money",
    "study",
    "school",
    "health",
    "travel",
    "music",
    "video",
    "photo",
    "photos",
    "book",
    "books",
    "research",
    "paper",
    "draft",
    "plan",
    "plans",
    "idea",
    "ideas",
    "design",
    "house",
    "garden",
    "career",
    "finance",
    "learning",
    "writing",
    "reading",
    "taxes",
    "ops",
];

/// What marks a project in a message: lowercase needles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub id: String,
    needles: Vec<String>,
    /// An id that is also an ordinary word: it counts only in backticks, or named again in a recent message.
    weak: bool,
}

pub fn candidates(list: &[Project]) -> Vec<Candidate> {
    list.iter()
        .filter(|p| p.id.chars().count() >= 3)
        .map(|p| {
            let id = p.id.to_lowercase();
            let weak = !id.contains(['-', '_', '.'])
                && !id.chars().any(|c| c.is_ascii_digit())
                && (id.chars().count() < 4 || GENERIC.contains(&id.as_str()));
            let mut needles = vec![id.clone()];
            if id.contains(['-', '_']) {
                needles.push(id.replace(['-', '_'], " "));
            }
            for path in p.home.iter().chain(&p.paths).filter(|_| !weak) {
                let name = path.trim_end_matches('/').rsplit('/').next().unwrap_or_default().to_lowercase();
                if name.chars().count() >= 4 && !GENERIC.contains(&name.as_str()) && !needles.contains(&name) {
                    needles.push(name);
                }
            }
            Candidate { id: p.id.clone(), needles, weak }
        })
        .collect()
}

fn word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '-' || c == '_'
}

/// Whether `needle` stands as a word of `hay` (both lowercase). Dashes and underscores belong to the word, so
/// `peace-guardian-ui` is not a mention of `peace-guardian`.
fn has_word(hay: &str, needle: &str) -> bool {
    hay.match_indices(needle).any(|(at, _)| {
        let before = hay[..at].chars().next_back();
        let after = hay[at + needle.len()..].chars().next();
        !before.is_some_and(word_char) && !after.is_some_and(word_char)
    })
}

/// The ids of the projects `text` names, in the order of `cands`. An ordinary-word id counts here only in backticks.
pub fn mentioned(cands: &[Candidate], text: &str) -> Vec<String> {
    let hay = text.to_lowercase();
    cands
        .iter()
        .filter(|c| c.needles.iter().any(|n| if c.weak { hay.contains(&format!("`{n}`")) } else { has_word(&hay, n) }))
        .map(|c| c.id.clone())
        .collect()
}

/// The ordinary-word ids `text` names without backticks.
fn weakly(cands: &[Candidate], text: &str) -> Vec<String> {
    let hay = text.to_lowercase();
    cands.iter().filter(|c| c.weak && c.needles.iter().any(|n| has_word(&hay, n))).map(|c| c.id.clone()).collect()
}

/// The projects visible to the one asking: the owner's own all, a person's only those shared with them, a bot guest's
/// none. Inference must not name a project the asker may not know of.
pub fn allowed(list: Vec<Project>, person: Option<&str>, shared: &[String]) -> Vec<Project> {
    match person {
        None => list,
        Some(p) if p.starts_with("bot-") => Vec::new(),
        Some(_) => list.into_iter().filter(|x| shared.contains(&x.id)).collect(),
    }
}

/// A thread's project state, kept between messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub projects: Vec<String>,
    /// `inferred` or `person`.
    pub by: String,
    /// Messages since the set last changed.
    pub since: u32,
    /// A correction the agent hasn't been told about yet: the set to announce with the next turn (empty: no project).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub announce: Option<Vec<String>>,
    /// The last user entry looked at, so a turn run again isn't counted or announced twice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last: Option<String>,
}

/// A change of the set, as the divider shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub from: Vec<String>,
    pub to: Vec<String>,
}

pub struct Decision {
    pub state: State,
    /// Set when the thread moved from one known set to another: apps show "Now on <project>".
    pub change: Option<Change>,
    /// The set the agent is to be told about with this turn (its tier-1 notes; empty means "no project"): the
    /// project just became current, or a correction is waiting.
    pub say: Option<Vec<String>>,
}

/// The set for a message. `now`: projects the message names; `recent`: those the last messages named (used only to
/// start a thread that has none yet). No state means the thread was never looked at: it is set without a divider.
pub fn decide(state: Option<&State>, now: &[String], recent: &[String]) -> Decision {
    let Some(state) = state else {
        let projects = if now.is_empty() { recent.to_vec() } else { now.to_vec() };
        let say = (!projects.is_empty()).then(|| projects.clone());
        return Decision {
            say,
            state: State { projects, by: "inferred".into(), since: 0, announce: None, last: None },
            change: None,
        };
    };
    let stays = now.is_empty() || now.iter().any(|p| state.projects.contains(p));
    // A thread that has a project holds it for a few messages; one with none takes the first one named.
    let held = state.since < COOLDOWN && !state.projects.is_empty();
    if stays || held {
        let next = State { since: state.since.saturating_add(1), ..state.clone() };
        return Decision { state: next, change: None, say: None };
    }
    let next =
        State { projects: now.to_vec(), by: "inferred".into(), since: 0, announce: None, last: state.last.clone() };
    Decision {
        change: Some(Change { from: state.projects.clone(), to: now.to_vec() }),
        say: Some(now.to_vec()),
        state: next,
    }
}

/// Inference and correction both read then write a thread's state; one at a time, so two messages can't undo each other.
static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn key(lane: &Lane) -> String {
    format!("thread-project:{}", lane.key())
}

pub fn read(store: &Store, lane: &Lane) -> Option<State> {
    serde_json::from_str(&store.kv_get(&key(lane))?).ok()
}

fn write(store: &Store, lane: &Lane, state: &State) {
    if let Err(error) = store.kv_set(&key(lane), &json!(state).to_string()) {
        tracing::warn!(error = format!("{error:#}"), "couldn't save a thread's project");
    }
}

/// The `projectChange` of a notice entry: `{from, to, by}`.
pub fn change_data(from: &[String], to: &[String], by: &str) -> Value {
    json!({"from": from, "to": to, "by": by})
}

/// The divider's words.
pub fn divider_text(to: &[String]) -> String {
    if to.is_empty() { "Now on no project".to_owned() } else { format!("Now on {}", to.join(", ")) }
}

/// Infers the thread's project set for the message `entry` (`text`), saves it, and says what changed. `recent`: the
/// user's last messages before this one. Asked again for the same entry (a turn run again) it changes nothing.
pub fn infer(store: &Store, lane: &Lane, known: &[Project], entry: &str, text: &str, recent: &[String]) -> Decision {
    let cands = candidates(known);
    let mut now = mentioned(&cands, text);
    let mut before: Vec<String> = Vec::new();
    for t in recent {
        for p in mentioned(&cands, t).into_iter().chain(weakly(&cands, t)) {
            if !before.contains(&p) {
                before.push(p);
            }
        }
    }
    // An ordinary word names a project only when the last messages did too.
    for p in weakly(&cands, text) {
        if before.contains(&p) && !now.contains(&p) {
            now.push(p);
        }
    }
    let _one = LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut state = read(store, lane);
    if let Some(s) = state.as_ref().filter(|s| s.last.as_deref() == Some(entry)) {
        return Decision { state: s.clone(), change: None, say: None };
    }
    let pending = state.as_mut().and_then(|s| s.announce.take());
    let mut decision = decide(state.as_ref(), &now, &before);
    decision.say = decision.say.or(pending);
    decision.state.last = Some(entry.to_owned());
    write(store, lane, &decision.state);
    decision
}

/// A person's correction: the set is theirs from now on, until the subject clearly moves. Returns the change when the
/// set differs from what the thread had.
pub fn correct(store: &Store, lane: &Lane, projects: Vec<String>) -> (State, Option<Change>) {
    let _one = LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let from = read(store, lane).map(|w| w.projects).unwrap_or_default();
    let announce = (from != projects).then(|| projects.clone());
    let state = State { projects, by: "person".into(), since: 0, announce, last: None };
    write(store, lane, &state);
    let change = (from != state.projects).then(|| Change { from, to: state.projects.clone() });
    (state, change)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(id: &str, paths: &[&str]) -> Project {
        Project { id: id.into(), paths: paths.iter().map(|p| (*p).to_owned()).collect(), ..Project::default() }
    }

    fn known() -> Vec<Candidate> {
        candidates(&[
            project("peace-guardian", &["/Users/me/Documents/guardian-app"]),
            project("peace-guardian-ui", &[]),
            project("kith", &["/Users/me/src"]),
            project("ab", &[]),
        ])
    }

    fn set(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn ids_names_and_folders_are_matched_as_words() {
        let c = known();
        assert_eq!(mentioned(&c, "fix the Peace-Guardian login"), set(&["peace-guardian"]));
        assert_eq!(mentioned(&c, "the peace guardian login"), set(&["peace-guardian"]));
        assert_eq!(mentioned(&c, "see guardian-app/README"), set(&["peace-guardian"]));
        assert_eq!(mentioned(&c, "peace-guardian-ui is slow"), set(&["peace-guardian-ui"]), "the longer id is its own");
        assert_eq!(mentioned(&c, "kith and peace-guardian"), set(&["peace-guardian", "kith"]));
        assert_eq!(mentioned(&c, "ab is too short; src is generic; kithara is another word"), set(&[]));
    }

    fn state(ids: &[&str], since: u32) -> State {
        State { projects: set(ids), by: "inferred".into(), since, announce: None, last: None }
    }

    #[test]
    fn an_unseen_thread_is_set_without_a_divider() {
        let d = decide(None, &set(&["kith"]), &[]);
        assert_eq!(d.state.projects, set(&["kith"]));
        assert!(d.change.is_none());
        assert_eq!(d.say, Some(set(&["kith"])));
        let d = decide(None, &[], &set(&["kith"]));
        assert_eq!(d.state.projects, set(&["kith"]), "the last messages start it");
        let d = decide(None, &[], &[]);
        assert!(d.state.projects.is_empty() && d.change.is_none() && d.say.is_none());
    }

    #[test]
    fn no_change_means_no_notice_and_no_injection() {
        let s = state(&["kith"], 10);
        for now in [vec![], set(&["kith"]), set(&["kith", "other"])] {
            let d = decide(Some(&s), &now, &[]);
            assert!(d.change.is_none() && d.say.is_none(), "{now:?}");
            assert_eq!(d.state.projects, s.projects);
            assert_eq!(d.state.since, 11);
        }
    }

    #[test]
    fn a_clear_move_is_one_notice_and_flapping_waits() {
        let d = decide(Some(&state(&["kith"], COOLDOWN)), &set(&["peace-guardian"]), &[]);
        let change = d.change.expect("moved");
        assert_eq!((change.from, change.to), (set(&["kith"]), set(&["peace-guardian"])));
        assert_eq!(d.say, Some(set(&["peace-guardian"])));
        assert_eq!((d.state.since, d.state.by.as_str()), (0, "inferred"));
        // Right after, a move back is held until the cooldown has passed.
        let back = decide(Some(&d.state), &set(&["kith"]), &[]);
        assert!(back.change.is_none() && back.state.projects == set(&["peace-guardian"]));
    }

    #[test]
    fn a_thread_with_no_project_takes_the_first_one_named_at_once() {
        let d = decide(Some(&state(&[], 0)), &set(&["kith"]), &[]);
        assert_eq!(d.change.map(|c| (c.from, c.to)), Some((vec![], set(&["kith"]))));
    }

    #[test]
    fn ordinary_words_name_a_project_only_in_backticks_or_twice() {
        let c = candidates(&[project("work", &["/Users/me/work-notes"]), project("ops", &[]), project("kith", &[])]);
        assert_eq!(mentioned(&c, "a lot of work today, and ops too"), set(&[]));
        assert_eq!(mentioned(&c, "see `work` and `ops`"), set(&["work", "ops"]));
        assert_eq!(weakly(&c, "more work"), set(&["work"]));
        let (store, dir) = store();
        let lane = Lane::main("b");
        let list = [project("work", &[]), project("kith", &[])];
        let first = infer(&store, &lane, &list, "e1", "plenty of work", &[]);
        assert_eq!(first.state.projects, set(&[]));
        let second = infer(&store, &lane, &list, "e2", "more work", &set(&["plenty of work"]));
        assert_eq!(second.state.projects, set(&["work"]), "named twice in a row it counts");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_person_sees_only_the_projects_shared_with_them() {
        let all = vec![project("kith", &[]), project("secret", &[])];
        let ids = |v: Vec<Project>| v.into_iter().map(|p| p.id).collect::<Vec<_>>();
        assert_eq!(ids(allowed(all.clone(), None, &[])), set(&["kith", "secret"]));
        assert_eq!(ids(allowed(all.clone(), Some("sam"), &set(&["kith"]))), set(&["kith"]));
        assert_eq!(ids(allowed(all.clone(), Some("sam"), &[])), set(&[]));
        assert_eq!(ids(allowed(all, Some("bot-ab12"), &set(&["kith"]))), set(&[]), "a guest gets none");
    }

    fn store() -> (Store, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("kith-project-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        (Store::open(&dir.join("db.sqlite")).unwrap(), dir)
    }

    #[test]
    fn a_correction_wins_until_the_subject_moves_and_is_announced_once() {
        let (store, dir) = store();
        let lane = Lane::in_thread("b", "root");
        let list = [project("kith", &[]), project("peace-guardian", &[])];
        let first = infer(&store, &lane, &list, "e1", "working on kith", &[]);
        assert_eq!(first.state.projects, set(&["kith"]));
        let (st, change) = correct(&store, &lane, set(&["peace-guardian"]));
        assert_eq!(st.by, "person");
        assert_eq!(change.map(|c| c.to), Some(set(&["peace-guardian"])));
        // The next turn tells the agent about it, once; the project stays whatever it says.
        let d = infer(&store, &lane, &list, "e2", "and the colours?", &[]);
        assert_eq!(d.say, Some(set(&["peace-guardian"])));
        assert_eq!((d.state.by.as_str(), d.change.is_none()), ("person", true));
        let d = infer(&store, &lane, &list, "e3", "peace-guardian again", &[]);
        assert!(d.say.is_none() && d.state.projects == set(&["peace-guardian"]));
        // After the cooldown a clear move by the subject takes it back to inference.
        for i in 0..COOLDOWN {
            infer(&store, &lane, &list, &format!("m{i}"), "more", &[]);
        }
        let d = infer(&store, &lane, &list, "e9", "kith needs a fix", &[]);
        assert!(d.change.is_some());
        assert_eq!(d.state.by, "inferred");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_correction_to_no_project_is_announced() {
        let (store, dir) = store();
        let lane = Lane::main("b");
        let list = [project("kith", &[])];
        infer(&store, &lane, &list, "e1", "kith", &[]);
        correct(&store, &lane, vec![]);
        let d = infer(&store, &lane, &list, "e2", "hello", &[]);
        assert_eq!(d.say, Some(vec![]));
        assert!(infer(&store, &lane, &list, "e3", "hello", &[]).say.is_none());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_turn_run_again_counts_and_announces_nothing_twice() {
        let (store, dir) = store();
        let lane = Lane::main("b");
        let list = [project("kith", &[]), project("peace-guardian", &[])];
        infer(&store, &lane, &list, "e1", "kith", &[]);
        for i in 0..COOLDOWN {
            infer(&store, &lane, &list, &format!("m{i}"), "more", &[]);
        }
        let moved = infer(&store, &lane, &list, "e5", "peace-guardian now", &[]);
        assert!(moved.change.is_some());
        let again = infer(&store, &lane, &list, "e5", "peace-guardian now", &[]);
        assert!(again.change.is_none() && again.say.is_none());
        assert_eq!(again.state, moved.state);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn state_is_kept_per_place() {
        let (store, dir) = store();
        let list = [project("kith", &[])];
        infer(&store, &Lane::in_thread("b", "one"), &list, "e1", "kith", &[]);
        assert!(read(&store, &Lane::in_thread("b", "two")).is_none());
        assert!(read(&store, &Lane::in_thread("b", "one")).is_some());
        std::fs::remove_dir_all(dir).ok();
    }
}
