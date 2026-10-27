//! Where each place a bot talks (its own chat, a thread or task, a group lane) works: an hq project or a plain
//! folder the user picked, so the agent loads that project's context the way `hq <agent> -p <project>` does in a
//! terminal. A place's pick is kept in the store under `lane_key("folder", bot, lane)`; a place without one takes
//! the bot's default project, then the bot's own `cwd` (its managed workspace, or the folder it was given).
//!
//! A group's members each work in their own pick for the group's lane, else their own default: a group has no
//! folder of its own.

use crate::agent::hq::{self, Project, ProjectExt as _};
use crate::agent::workspace;
use crate::store::{BotConfig, Lane, Store, lane_key};
use anyhow::{Context as _, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Test hosts add folders (a temp dir is outside the home folder) the pickers may browse and set, separated like `PATH`.
pub const EXTRA_ROOTS_ENV: &str = "KITH_SERVE_FOLDER_ROOTS";

/// What the user picked for a place.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Pick {
    /// A registered hq project, by id.
    Project(String),
    /// A folder by absolute path; the kernel finds the registered project from it, if any.
    Folder(String),
}

impl Pick {
    fn key(bot: &str, lane: &Lane) -> String {
        lane_key("folder", bot, lane)
    }

    /// The pick stored for `bot`'s `lane`, if any.
    pub fn load(store: &Store, bot: &str, lane: &Lane) -> Option<Self> {
        serde_json::from_str(&store.kv_get(&Self::key(bot, lane))?).ok()
    }

    /// Stores the pick, or with `None` forgets it so the place inherits again.
    pub fn save(store: &Store, bot: &str, lane: &Lane, pick: Option<&Self>) -> Result<()> {
        match pick {
            Some(p) => store.kv_set(&Self::key(bot, lane), &serde_json::to_string(p)?),
            None => store.kv_delete(&Self::key(bot, lane)),
        }
    }
}

/// What a client asked for in `setPlaceFolder` (and the optional start folder of `send`).
#[derive(Debug, PartialEq, Eq)]
pub enum Change {
    /// Neither a project, a folder nor `clear`.
    Nothing,
    Clear,
    Set(Pick),
}

impl Change {
    pub fn from_args(b: &Value) -> Result<Self> {
        let text = |k: &str| b[k].as_str().map(str::trim).filter(|s| !s.is_empty());
        let (project, folder, clear) = (text("project"), text("folder"), b["clear"] == true);
        Ok(match (project, folder, clear) {
            (None, None, false) => Self::Nothing,
            (None, None, true) => Self::Clear,
            (Some(id), None, false) => Self::Set(Pick::Project(id.to_owned())),
            (None, Some(path), false) => Self::Set(Pick::Folder(path.to_owned())),
            _ => bail!("give one of `project`, `folder` or `clear`"),
        })
    }
}

/// A place's folder once its pick, the bot's default and its `cwd` are weighed.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Effective {
    /// None: the bot's `cwd`.
    pub pick: Option<Pick>,
    /// The place has no pick of its own.
    pub inherited: bool,
}

/// The place's own pick wins, then the bot's default project, then the bot's `cwd`.
pub fn choose(place: Option<Pick>, bot_project: Option<&str>) -> Effective {
    match (place, bot_project.filter(|p| !p.is_empty())) {
        (Some(pick), _) => Effective { pick: Some(pick), inherited: false },
        (None, Some(id)) => Effective { pick: Some(Pick::Project(id.to_owned())), inherited: true },
        (None, None) => Effective { pick: None, inherited: true },
    }
}

/// What a place without a pick works in, to tell whether a bot's edit moved it: the project, else the `cwd`.
pub fn default_of(cfg: &BotConfig) -> (Option<&str>, Option<&str>) {
    match cfg.project.as_deref().filter(|p| !p.is_empty()) {
        Some(project) => (Some(project), None),
        None => (None, Some(&cfg.cwd)),
    }
}

pub fn effective(store: &Store, cfg: &BotConfig, lane: &Lane) -> Effective {
    choose(Pick::load(store, &cfg.id, lane), cfg.project.as_deref())
}

/// What an agent for a place is started with.
#[derive(Debug, PartialEq, Eq)]
pub struct Target {
    /// The agent's working folder: the folder it is spawned in and the ACP session's `cwd`.
    pub cwd: String,
    /// The project hq is asked for (`-p`), when the place has one.
    pub project: Option<String>,
    /// The folder was picked, so it beats the working folder hq names.
    pub explicit: bool,
    /// Why a picked folder isn't used, for the chat.
    pub notice: Option<String>,
}

/// Where to run for `pick`, given the projects hq listed (or why it couldn't). Falls back to `fallback` (the bot's
/// `cwd`) when the picked folder isn't on this computer, and says so.
pub fn locate(pick: Option<&Pick>, fallback: &str, projects: Result<&[Project], &str>) -> Target {
    let plain = |notice: Option<String>, project: Option<String>| Target {
        cwd: fallback.to_owned(),
        project,
        explicit: false,
        notice,
    };
    match pick {
        None => plain(None, None),
        Some(Pick::Folder(path)) if Path::new(path).is_dir() => {
            Target { cwd: path.clone(), project: None, explicit: true, notice: None }
        }
        Some(Pick::Folder(path)) => plain(
            Some(format!("The folder {path} isn't on this computer any more — working in {fallback} instead.")),
            None,
        ),
        Some(Pick::Project(id)) => match projects {
            Err(why) => plain(
                Some(format!("Couldn't look up project {id} ({why}) — working in {fallback} instead.")),
                Some(id.clone()),
            ),
            Ok(list) => match list.iter().find(|p| p.id == *id) {
                None => plain(Some(format!("{id} isn't a project hq knows — working in {fallback} instead.")), None),
                Some(project) => match project.folder() {
                    Some(cwd) => Target { cwd, project: Some(id.clone()), explicit: true, notice: None },
                    // hq still loads the project's context from its registry.
                    None => plain(
                        Some(format!("Project {id} has no folder on this computer — working in {fallback} instead.")),
                        Some(id.clone()),
                    ),
                },
            },
        },
    }
}

/// [`locate`] for `bot`'s `lane`, asking hq for its projects when the place needs one.
pub async fn resolve(store: &Store, cfg: &BotConfig, lane: &Lane) -> Target {
    let eff = effective(store, cfg, lane);
    let listed = match eff.pick {
        Some(Pick::Project(_)) => hq::projects(false).await.map_err(|e| format!("{e:#}")),
        _ => Ok(Vec::new()),
    };
    locate(eff.pick.as_ref(), &cfg.cwd, listed.as_deref().map_err(String::as_str))
}

/// The place's folder as clients read it: `{kind: project|folder|workspace, project?, path, name, inherited}`.
/// `path` is null while the project's folder is unknown (no list yet), and `missing` is true when it has none here.
pub fn describe(eff: &Effective, cfg: &BotConfig, projects: &[Project]) -> Value {
    let base = |name: &str| Path::new(name).file_name().map_or_else(|| name.to_owned(), |n| n.to_string_lossy().into());
    match &eff.pick {
        Some(Pick::Project(id)) => {
            let found = projects.iter().find(|p| p.id == *id);
            let mut v = json!({
                "kind": "project", "project": id, "path": found.and_then(Project::folder), "name": id,
                "inherited": eff.inherited,
            });
            if found.is_some_and(|p| p.folder().is_none()) {
                v["missing"] = true.into();
            }
            v
        }
        Some(Pick::Folder(path)) => {
            let mut v = json!({"kind": "folder", "path": path, "name": base(path), "inherited": eff.inherited});
            if !Path::new(path).is_dir() {
                v["missing"] = true.into();
            }
            v
        }
        None if workspace::is_managed(cfg) => {
            json!({"kind": "workspace", "path": cfg.cwd, "name": "Workspace", "inherited": true})
        }
        None => json!({"kind": "folder", "path": cfg.cwd, "name": base(&cfg.cwd), "inherited": true}),
    }
}

/// [`describe`] for `bot`'s `lane`, with the projects hq last listed.
pub fn describe_cached(store: &Store, cfg: &BotConfig, lane: &Lane) -> Value {
    describe(&effective(store, cfg, lane), cfg, &hq::cached_projects())
}

/// A folder the pickers may browse and a place may be set to.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct Root {
    pub name: String,
    pub path: PathBuf,
}

/// Where `/Volumes` lists the drives mounted on a Mac.
const VOLUMES: &str = "/Volumes";

/// The user's home and each mounted drive: nothing else is browsable. Paths are resolved (no symlinks).
pub fn roots() -> Vec<Root> {
    let mut roots = Vec::new();
    if let Some(home) = dirs::home_dir().and_then(|h| h.canonicalize().ok()) {
        roots.push(Root { name: "Home".into(), path: home });
    }
    roots.extend(volumes());
    for extra in std::env::var_os(EXTRA_ROOTS_ENV).iter().flat_map(std::env::split_paths) {
        if let Ok(path) = extra.canonicalize() {
            let name = path.file_name().map_or_else(|| "Folder".into(), |n| n.to_string_lossy().into_owned());
            roots.push(Root { name, path });
        }
    }
    roots.dedup_by(|a, b| a.path == b.path);
    roots
}

/// The drives under `/Volumes`, not counting the one the system boots from (a link to `/`).
fn volumes() -> Vec<Root> {
    let Ok(entries) = std::fs::read_dir(VOLUMES) else { return Vec::new() };
    let mut volumes: Vec<Root> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let path = e.path().canonicalize().ok()?;
            let boot = path == Path::new("/");
            (path.is_dir() && !boot).then(|| Root { name: e.file_name().to_string_lossy().into_owned(), path })
        })
        .collect();
    volumes.sort_by_key(|v| v.name.to_lowercase());
    volumes
}

/// Resolves `path` (symlinks too) and checks it is a folder inside one of the `roots`.
pub fn check_within(path: &Path, roots: &[Root]) -> Result<PathBuf> {
    let canon = path.canonicalize().with_context(|| format!("{} isn't a folder on this computer", path.display()))?;
    if !canon.is_dir() {
        bail!("{} isn't a folder", path.display());
    }
    if !roots.iter().any(|r| canon.starts_with(&r.path)) {
        bail!("{} is outside your home folder and the drives mounted on this computer", path.display());
    }
    Ok(canon)
}

pub fn check(path: &Path) -> Result<PathBuf> {
    check_within(path, &roots())
}

/// System folders `listDirs` leaves out at the top of a root, unless the client asks for hidden ones too.
const SYSTEM_DIRS: &[&str] = &[
    "Library",
    "Applications",
    "System",
    "private",
    "cores",
    "usr",
    "bin",
    "sbin",
    "opt",
    "dev",
    "etc",
    "var",
    "tmp",
    "$RECYCLE.BIN",
    "System Volume Information",
    "lost+found",
];

fn hidden(name: &str, at_root: bool) -> bool {
    name.starts_with('.') || (at_root && SYSTEM_DIRS.contains(&name))
}

/// Blocking (`std::fs`): call through `spawn_blocking`. `listDirs`: the folders in `path` (default: home), inside
/// the browsable roots, each with the project registered at it.
pub fn list_dirs(path: Option<&str>, show_hidden: bool, projects: &[Project]) -> Result<Value> {
    list_dirs_in(&roots(), path, show_hidden, projects)
}

fn list_dirs_in(roots: &[Root], path: Option<&str>, show_hidden: bool, projects: &[Project]) -> Result<Value> {
    let by_path = project_paths(projects);
    let marked = |p: &Path| by_path.get(&*p.to_string_lossy()).cloned();
    let entry =
        |name: &str, p: &Path| json!({"name": name, "path": p, "isGit": p.join(".git").exists(), "project": marked(p)});
    let root_values = || roots.iter().map(|r| json!({"name": r.name, "path": r.path})).collect::<Vec<_>>();
    // `/Volumes` itself lists the drives.
    if path.is_some_and(|p| Path::new(p) == Path::new(VOLUMES)) {
        let dirs: Vec<Value> = volumes().iter().map(|v| entry(&v.name, &v.path)).collect();
        return Ok(
            json!({"path": VOLUMES, "parent": null, "isGit": false, "dirs": dirs, "roots": root_values(), "project": null}),
        );
    }
    let dir = match path {
        Some(p) => PathBuf::from(p),
        None => dirs::home_dir().ok_or_else(|| anyhow!("no home directory"))?,
    };
    let dir = check_within(&dir, roots)?;
    let at_root = roots.iter().any(|r| r.path == dir);
    let mut dirs: Vec<Value> = std::fs::read_dir(&dir)
        .with_context(|| format!("listing {}", dir.display()))?
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            (show_hidden || !hidden(&name, at_root)).then(|| entry(&name, &e.path()))
        })
        .collect();
    dirs.sort_by_key(|d| d["name"].as_str().unwrap_or_default().to_lowercase());
    let parent = dir.parent().filter(|p| roots.iter().any(|r| p.starts_with(&r.path)));
    Ok(json!({
        "path": dir, "parent": parent, "isGit": dir.join(".git").exists(), "dirs": dirs,
        "roots": root_values(), "project": marked(&dir),
    }))
}

/// Folder -> the project registered there, by the path as registered and as it resolves.
fn project_paths(projects: &[Project]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for project in projects {
        for path in project.all_paths() {
            map.insert(path.clone(), project.id.clone());
            if let Ok(canon) = Path::new(path).canonicalize() {
                map.insert(canon.to_string_lossy().into_owned(), project.id.clone());
            }
        }
    }
    map
}

const RECENT_KEY: &str = "folders.recent";
const RECENT_CAP: usize = 12;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
struct Recent {
    path: String,
    #[serde(default)]
    project: Option<String>,
}

impl Recent {
    fn same(&self, other: &Self) -> bool {
        match (&self.project, &other.project) {
            (Some(a), Some(b)) => a == b,
            (None, None) => self.path == other.path,
            _ => false,
        }
    }
}

fn recents(store: &Store) -> Vec<Recent> {
    store.kv_get(RECENT_KEY).and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default()
}

/// Notes that a place was set to `pick` (at `path`, its folder), newest first, at most 12.
pub fn record_recent(store: &Store, pick: &Pick, path: &str) -> Result<()> {
    let new = match pick {
        Pick::Project(id) => Recent { path: path.to_owned(), project: Some(id.clone()) },
        Pick::Folder(p) => Recent { path: p.clone(), project: None },
    };
    let mut list = recents(store);
    list.retain(|r| !r.same(&new));
    list.insert(0, new);
    list.truncate(RECENT_CAP);
    store.kv_set(RECENT_KEY, &serde_json::to_string(&list)?)
}

/// `recentFolders`: `[{path, name, project}]`, newest first, without the folders that are gone.
pub fn recent_folders(store: &Store) -> Vec<Value> {
    recents(store)
        .into_iter()
        .filter(|r| Path::new(&r.path).is_dir())
        .map(|r| {
            let name = r.project.clone().unwrap_or_else(|| {
                Path::new(&r.path).file_name().map_or_else(|| r.path.clone(), |n| n.to_string_lossy().into_owned())
            });
            json!({"path": r.path, "name": name, "project": r.project})
        })
        .collect()
}

/// A project id as `hq project add` takes it: lowercase words joined by single dashes.
pub fn valid_project_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.split('-').all(|w| !w.is_empty() && w.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()))
}

/// A project as the API lists it: `{id, path, paths, parent, exists}`; `path` is the folder to work in.
pub fn project_value(p: &Project) -> Value {
    let folder = p.folder();
    let paths: Vec<&String> = p.all_paths().collect();
    json!({"id": p.id, "path": folder, "paths": paths, "parent": p.parent, "exists": folder.is_some()})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(id: &str, home: Option<&str>, paths: &[&str]) -> Project {
        Project {
            id: id.into(),
            home: home.map(str::to_owned),
            paths: paths.iter().map(|p| (*p).to_owned()).collect(),
            ..Project::default()
        }
    }

    fn temp_store() -> Store {
        Store::open(&temp("db").join("t.db")).unwrap()
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kith-folder-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    #[test]
    fn the_place_beats_the_bots_project_which_beats_its_cwd() {
        let mine = Pick::Folder("/work/app".into());
        assert_eq!(choose(Some(mine.clone()), Some("hq")), Effective { pick: Some(mine), inherited: false });
        assert_eq!(choose(None, Some("hq")), Effective { pick: Some(Pick::Project("hq".into())), inherited: true });
        assert_eq!(choose(None, None), Effective { pick: None, inherited: true });
        assert_eq!(choose(None, Some("")), Effective { pick: None, inherited: true }, "an empty project is none");
    }

    #[test]
    fn a_pick_reads_back_as_stored_and_a_clear_inherits_again() {
        let store = temp_store();
        let lane = Lane::in_thread("b", "root");
        assert_eq!(Pick::load(&store, "b", &lane), None);
        Pick::save(&store, "b", &lane, Some(&Pick::Project("hq".into()))).unwrap();
        assert_eq!(Pick::load(&store, "b", &lane), Some(Pick::Project("hq".into())));
        assert_eq!(Pick::load(&store, "b", &Lane::main("b")), None, "another place keeps none");
        assert_eq!(store.kv_get("lane.folder.b@b/root").unwrap(), r#"{"project":"hq"}"#);
        Pick::save(&store, "b", &lane, None).unwrap();
        assert_eq!(Pick::load(&store, "b", &lane), None);
    }

    #[test]
    fn a_change_names_one_thing() {
        let change = |v: Value| Change::from_args(&v);
        assert_eq!(change(json!({})).unwrap(), Change::Nothing);
        assert_eq!(change(json!({"clear": true})).unwrap(), Change::Clear);
        assert_eq!(change(json!({"project": "hq"})).unwrap(), Change::Set(Pick::Project("hq".into())));
        assert_eq!(change(json!({"folder": "/x"})).unwrap(), Change::Set(Pick::Folder("/x".into())));
        assert!(change(json!({"project": "hq", "folder": "/x"})).is_err());
        assert!(change(json!({"project": "hq", "clear": true})).is_err());
    }

    #[test]
    fn locating_a_pick_falls_back_with_a_notice_when_its_folder_is_gone() {
        let here = temp("locate");
        let registry =
            [project("hq", Some("/nowhere/hq"), &[here.to_str().unwrap()]), project("lost", None, &["/nowhere"])];
        let target = |pick: &Pick| locate(Some(pick), "/bot", Ok(&registry));

        // The home that isn't here is skipped for the first path that is.
        let hq = target(&Pick::Project("hq".into()));
        assert_eq!(
            hq,
            Target { cwd: here.to_string_lossy().into(), project: Some("hq".into()), explicit: true, notice: None }
        );
        // Registered, no folder here: the bot's folder, -p still passed.
        let lost = target(&Pick::Project("lost".into()));
        assert_eq!((lost.cwd.as_str(), lost.project.as_deref(), lost.explicit), ("/bot", Some("lost"), false));
        assert!(lost.notice.unwrap().contains("no folder on this computer"));
        // Not registered at all: no -p, which would only fail.
        let unknown = target(&Pick::Project("gone".into()));
        assert_eq!((unknown.cwd.as_str(), unknown.project), ("/bot", None));
        assert!(unknown.notice.is_some());
        // A plain folder: its own cwd and no project; deleted: the fallback.
        let folder = target(&Pick::Folder(here.to_string_lossy().into()));
        assert_eq!((folder.cwd, folder.project, folder.explicit), (here.to_string_lossy().into(), None, true));
        let deleted = target(&Pick::Folder("/nowhere/at/all".into()));
        assert_eq!((deleted.cwd.as_str(), deleted.explicit), ("/bot", false));
        assert!(deleted.notice.unwrap().contains("isn't on this computer"));
        // No pick: today's behaviour. hq failing: the fallback, project kept.
        assert_eq!(
            locate(None, "/bot", Ok(&[])),
            Target { cwd: "/bot".into(), project: None, explicit: false, notice: None }
        );
        let failed = locate(Some(&Pick::Project("hq".into())), "/bot", Err("hq isn't installed"));
        assert_eq!((failed.cwd.as_str(), failed.project.as_deref()), ("/bot", Some("hq")));
        std::fs::remove_dir_all(here).unwrap();
    }

    #[test]
    fn a_folder_is_browsable_inside_a_root_only() {
        let (inside, outside) = (temp("inside"), temp("outside"));
        std::fs::create_dir_all(inside.join("a/b")).unwrap();
        let roots = vec![Root { name: "Inside".into(), path: inside.clone() }];
        assert_eq!(check_within(&inside.join("a/b"), &roots).unwrap(), inside.join("a/b"));
        assert!(check_within(&outside, &roots).unwrap_err().to_string().contains("outside"));
        assert!(check_within(&inside.join("missing"), &roots).is_err());
        // `..` and a link out are resolved before the check.
        assert!(check_within(&inside.join("a/../.."), &roots).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, inside.join("link")).unwrap();
            assert!(check_within(&inside.join("link"), &roots).is_err(), "a symlink doesn't lead out");
        }
        let file = inside.join("file");
        std::fs::write(&file, "x").unwrap();
        assert!(check_within(&file, &roots).is_err());
        std::fs::remove_dir_all(inside).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn dot_and_system_folders_stay_out_of_the_listing_unless_asked() {
        assert!(hidden(".git", false) && hidden(".Trash", true));
        assert!(hidden("Library", true) && !hidden("Library", false), "only at the top of a root");
        assert!(!hidden("src", true));
    }

    #[test]
    fn project_ids_are_lowercase_kebab() {
        for ok in ["hq", "kith-app", "a1-b2"] {
            assert!(valid_project_id(ok), "{ok}");
        }
        for bad in ["", "Hq", "-a", "a-", "a--b", "a_b", "a b", "../x"] {
            assert!(!valid_project_id(bad), "{bad}");
        }
    }

    #[test]
    fn recents_keep_twelve_newest_first_and_drop_gone_folders() {
        let store = temp_store();
        let dir = temp("recent");
        for i in 0..14 {
            let sub = dir.join(format!("d{i}"));
            std::fs::create_dir_all(&sub).unwrap();
            record_recent(&store, &Pick::Folder(sub.to_string_lossy().into()), "").unwrap();
        }
        record_recent(&store, &Pick::Project("hq".into()), dir.to_str().unwrap()).unwrap();
        // Setting a folder again moves it up instead of adding a copy.
        record_recent(&store, &Pick::Folder(dir.join("d5").to_string_lossy().into()), "").unwrap();
        let list = recent_folders(&store);
        assert_eq!(list.len(), 12);
        assert_eq!(list[0]["name"], "d5");
        assert_eq!((list[1]["name"].as_str(), list[1]["project"].as_str()), (Some("hq"), Some("hq")));
        assert_eq!(list.iter().filter(|r| r["name"] == "d5").count(), 1);
        std::fs::remove_dir_all(dir.join("d13")).unwrap();
        assert!(recent_folders(&store).iter().all(|r| r["name"] != "d13"), "a deleted folder drops out");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn listing_marks_projects_hides_dot_dirs_and_refuses_outside_paths() {
        let dir = temp("list");
        for sub in ["app", ".hidden", "plain"] {
            std::fs::create_dir_all(dir.join(sub)).unwrap();
        }
        std::fs::create_dir_all(dir.join("app/.git")).unwrap();
        let projects = [project("app", None, &[dir.join("app").to_str().unwrap()])];
        let roots = [Root { name: "List".into(), path: dir.clone() }];
        let list =
            |path: &Path, hidden: bool, projects: &[Project]| list_dirs_in(&roots, path.to_str(), hidden, projects);
        let listing = list(&dir, false, &projects).unwrap();
        let names: Vec<&str> =
            listing["dirs"].as_array().unwrap().iter().map(|d| d["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["app", "plain"]);
        assert_eq!(listing["dirs"][0]["project"], "app");
        assert_eq!(listing["dirs"][0]["isGit"], true);
        assert_eq!(listing["dirs"][1]["project"], Value::Null);
        assert_eq!(listing["parent"], Value::Null, "a root has no parent to climb to");
        assert!(listing["roots"].as_array().unwrap().iter().any(|r| r["path"] == dir.to_str().unwrap()));
        let all = list(&dir, true, &projects).unwrap();
        assert_eq!(all["dirs"].as_array().unwrap().len(), 3);
        let app = list(&dir.join("app"), false, &projects).unwrap();
        assert_eq!((app["parent"].as_str(), app["project"].as_str()), (dir.to_str(), Some("app")));
        assert!(list(Path::new("/etc"), false, &[]).unwrap_err().to_string().contains("outside"));
        assert!(list(Path::new("/"), false, &[]).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
