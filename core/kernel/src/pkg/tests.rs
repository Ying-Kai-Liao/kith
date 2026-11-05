//! The rules of `kith-pkg resolve`, one at a time. The whole line, against the Python it replaces, is the golden record
//! (`kernel/tests/difftest/golden/pkg-resolve.json`); these are the parts that need no process to be told apart.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use super::cli;
use super::py::{read_text, repr, strip};
use super::resolve::resolve;
use super::text::{frontmatter, has_needs, needs_list, placeholders, skill_refs, split_needs};
use super::{Ctx, fingerprint, sha1};

static N: AtomicU32 = AtomicU32::new(0);

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "kith-pkg-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("kernel")).unwrap();
        fs::write(dir.join("kernel/PROTOCOL.md"), "# protocol\n").unwrap();
        Self(dir)
    }

    fn ctx(&self) -> Ctx {
        Ctx {
            kernel: self.0.join("kernel"),
            agents: self.0.join("agents"),
            state: self.0.join("state"),
            store: self.0.join("state/store"),
            github: self.0.join("nowhere").to_string_lossy().into_owned(),
            python: "python3".to_owned(),
        }
    }

    fn put(&self, rel: &str, text: &str) {
        let p = self.0.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }

    fn agent(&self, name: &str) {
        self.put(
            &format!("agents/{name}/role/agent.md"),
            &format!("---\nname: {name}\ndescription: test agent\n---\nYou are {name}.\n"),
        );
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn resolved(ctx: &Ctx, agent: &str) -> (PathBuf, String) {
    resolve(ctx, agent, &mut String::new()).unwrap()
}

fn set_mtime(p: &Path, secs: u64, nanos: u32) {
    let f = fs::File::options().write(true).open(p).unwrap();
    f.set_modified(UNIX_EPOCH + Duration::new(secs, nanos))
        .unwrap();
}

#[test]
fn sha1_is_sha1() {
    assert_eq!(sha1::hex(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
    assert_eq!(
        sha1::hex(b"abc"),
        "a9993e364706816aba3e25717850c26c9cd0d89d"
    );
    assert_eq!(
        sha1::hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
        "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
    );
    // 56 bytes: the length no longer fits in the block that holds the padding byte; 64 and 1000: whole and many blocks
    assert_eq!(
        sha1::hex(&[b'a'; 56]),
        "c2db330f6083854c99d4b5bfb6e8f29f201be699"
    );
    assert_eq!(
        sha1::hex(&[b'a'; 64]),
        "0098ba824b5c16427bd7a1122a5a442a25ec644d"
    );
    assert_eq!(
        sha1::hex(&[b'a'; 1000]),
        "291e9a6c66994949b57ba5e650361e98fc36b1ba"
    );
}

const VECTOR_FILES: [(&str, &str, u64, u32); 10] = [
    (
        "agents/echo/role/agent.md",
        "---\nname: echo\ndescription: d\n---\nhi\n",
        1_700_000_000,
        123_456_789,
    ),
    (
        "agents/echo/role/notes.md",
        "notes \u{e9}\n",
        1_700_000_001,
        0,
    ),
    ("agents/echo/role/sub/x.txt", "x", 1_600_000_000, 1),
    (
        "agents/echo/role/sub/.DS_Store",
        "skipped",
        1_700_000_002,
        0,
    ),
    ("agents/echo/role/memory/m.md", "state", 1_700_000_003, 0),
    (
        "agents/echo/role/sub/memory/keep.md",
        "nested memory is package",
        1_700_000_004,
        0,
    ),
    (
        "agents/echo/role/node_modules/n.js",
        "skipped",
        1_700_000_005,
        0,
    ),
    (
        "agents/plain/agent.md",
        "---\nname: plain\ndescription: d\n---\nhi\n",
        1_700_000_006,
        0,
    ),
    ("agents/plain/z.txt", "zz", 1_700_000_007, 0),
    ("kernel/PROTOCOL.md", "# protocol\n", 1_700_000_008, 0),
];

/// What the hash is over for the counted files of `VECTOR_FILES`, in `os.walk` order: `(label, size, mtime ns)`.
fn vector_entries(names: &[(&str, &str)]) -> Vec<(Vec<u8>, u64, i128)> {
    names
        .iter()
        .map(|(rel, label)| {
            let (_, text, secs, nanos) = VECTOR_FILES.iter().find(|f| f.0 == *rel).unwrap();
            (
                label.as_bytes().to_vec(),
                text.len() as u64,
                i128::from(*secs) * 1_000_000_000 + i128::from(*nanos),
            )
        })
        .collect()
}

const ROLE_FILES: [(&str, &str); 5] = [
    ("agents/echo/role/agent.md", "agent.md"),
    ("agents/echo/role/notes.md", "notes.md"),
    ("agents/echo/role/sub/x.txt", "sub/x.txt"),
    ("agents/echo/role/sub/memory/keep.md", "sub/memory/keep.md"),
    ("kernel/PROTOCOL.md", "PROTOCOL.md"),
];
const PLAIN_FILES: [(&str, &str); 3] = [
    ("agents/plain/agent.md", "agent.md"),
    ("agents/plain/z.txt", "z.txt"),
    ("kernel/PROTOCOL.md", "PROTOCOL.md"),
];

/// The hash of a fixed package, as `kith-pkg` printed it for the same names, sizes, mtimes (to the nanosecond) and place of
/// the agent's life. Pure, so the place is a fixed string.
#[test]
fn fingerprint_hash_matches_the_python() {
    let root = "/tmp/kith-pkg-fp-vector";
    assert_eq!(
        super::fingerprint_of(&format!("{root}/agents/echo"), &vector_entries(&ROLE_FILES)),
        "5ec0694041"
    );
    assert_eq!(
        super::fingerprint_of(&format!("{root}/state"), &vector_entries(&PLAIN_FILES)),
        "d508ed54ec"
    );
}

/// The same files on disk, in a folder of this test's own: what is walked, skipped and stat-ed gives the same entries.
#[test]
fn fingerprint_walks_the_package_the_python_way() {
    let t = Tmp::new();
    for (rel, text, secs, nanos) in VECTOR_FILES {
        let p = t.0.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, text).unwrap();
        set_mtime(&p, secs, nanos);
    }
    fs::create_dir_all(t.0.join("state")).unwrap();
    let ctx = t.ctx();
    let root = t.0.display().to_string();
    assert_eq!(
        fingerprint(&ctx, &t.0.join("agents/echo/role")).unwrap(),
        super::fingerprint_of(&format!("{root}/agents/echo"), &vector_entries(&ROLE_FILES))
    );
    assert_eq!(
        fingerprint(&ctx, &t.0.join("agents/plain")).unwrap(),
        super::fingerprint_of(&format!("{root}/state"), &vector_entries(&PLAIN_FILES))
    );
}

#[test]
fn a_fetch_folder_is_private() {
    use std::os::unix::fs::PermissionsExt as _;
    let d = super::fetch::TempDir::new().unwrap();
    let mode = fs::metadata(&d.0).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700);
}

#[test]
fn rmtree_leaves_a_link_alone() {
    let t = Tmp::new();
    fs::create_dir_all(t.0.join("real/inner")).unwrap();
    std::os::unix::fs::symlink("real", t.0.join("link")).unwrap();
    super::py::rmtree(&t.0.join("link"));
    assert!(fs::symlink_metadata(t.0.join("link")).is_ok() && t.0.join("real/inner").is_dir());
    super::py::rmtree(&t.0.join("real"));
    assert!(!t.0.join("real").exists());
}

#[test]
fn pids_read_as_python_ints_and_probe_like_os_kill() {
    use super::py::py_int;
    assert_eq!(py_int(" +12 "), Some(12));
    assert_eq!(py_int("1_000"), Some(1000));
    assert_eq!(py_int("-3"), Some(-3));
    for bad in ["", "_1", "1_", "1__0", "x", "1x", "+", "--1"] {
        assert_eq!(py_int(bad), None, "{bad:?}");
    }
    let t = Tmp::new();
    let me = std::process::id();
    let cases = [
        (format!("{me}.a"), true),
        (" +1.b".to_owned(), true),
        ("0.c".to_owned(), true),
        ("999999.d".to_owned(), false),
        ("junk.e".to_owned(), false),
    ];
    for (name, alive) in cases {
        let dir = t.0.join(format!("snap-{name}"));
        t.put(&format!("snap-{name}/.runs/{name}"), "");
        assert_eq!(super::store::running(&dir), alive, "{name}");
        // a record of a finished run is cleared
        assert_eq!(dir.join(".runs").join(&name).exists(), alive, "{name}");
    }
}

#[test]
fn paths_are_spelled_as_pythons_path_spells_them() {
    let env = |k: &str| match k {
        "KITH_AGENTS" => Some("/a//b/./c/".to_owned()),
        "KITH_STATE" => Some("/s/./t//".to_owned()),
        _ => None,
    };
    let p = crate::paths::paths(Path::new("/h"), Path::new("/k"), &env);
    assert_eq!(p.agents, PathBuf::from("/a/b/c"));
    assert_eq!(p.state, PathBuf::from("/s/t"));
}

#[test]
fn frontmatter_reads_fields_the_way_the_regex_does() {
    let f = frontmatter(
        "---\nname: x\ndescription:\nname: next line is the value\neffort:   \n---\nbody\n",
    );
    // `\s*` runs over the newline: an empty value takes the next line
    assert_eq!(f.get("description"), Some("name: next line is the value"));
    assert_eq!(f.get("name"), Some("x"));
    assert_eq!(f.get("effort"), Some(""));
    assert_eq!(f.body, "body\n");
    let none = frontmatter("no frontmatter\n---\n");
    assert_eq!(none.get("name"), None);
    assert_eq!(none.body, "no frontmatter\n---\n");
    // the first `\n---` closes it, even inside a longer line
    let early = frontmatter("---\na: 1\n----\nb: 2\n---\nrest\n");
    assert_eq!(early.get("a"), Some("1"));
    assert_eq!(early.get("b"), None);
    assert_eq!(early.body, "-\nb: 2\n---\nrest\n");
}

#[test]
fn read_text_reads_lines_as_python_does() {
    let t = Tmp::new();
    t.put("a.md", "one\r\ntwo\rthree\n");
    assert_eq!(read_text(&t.0.join("a.md")).unwrap(), "one\ntwo\nthree\n");
    fs::write(t.0.join("b.md"), [0xff, 0xfe]).unwrap();
    let e = read_text(&t.0.join("b.md")).unwrap_err();
    assert_eq!(
        e.msg,
        "'utf-8' codec can't decode byte 0xff in position 0: invalid start byte"
    );
    let e = read_text(&t.0.join("gone.md")).unwrap_err();
    assert!(
        e.msg.starts_with("[Errno 2] No such file or directory: '"),
        "{}",
        e.msg
    );
}

#[test]
fn small_regexes() {
    assert_eq!(
        needs_list("a\nneeds:\n  [x , y,\n z]\n"),
        Some("x , y,\n z")
    );
    assert_eq!(needs_list("needs:\n  - a\n"), None);
    assert!(has_needs("x\nneeds:\n  - a\n"));
    assert!(!has_needs("  needs: [a]\n"));
    assert_eq!(split_needs(" a , ,b,"), vec!["a", "b"]);
    let mut p = placeholders("{{A}} {{{{B_C}}}} {{lower}} {{A}} {{ }} {{D");
    p.sort();
    assert_eq!(p, vec!["A", "B_C"]);
    assert_eq!(
        skill_refs("`skills/a/SKILL.md` `b/SKILL.md x` `/SKILL.md` `c/SKILL.md`"),
        vec!["skills/a/SKILL.md", "c/SKILL.md"]
    );
    assert_eq!(repr("it's"), "\"it's\"");
    assert_eq!(repr("a'b\"c"), "'a\\'b\"c'");
    assert_eq!(strip("\u{1c} x \u{a0}\n"), "x");
}

#[test]
fn resolve_snapshots_once_and_follows_edits() {
    let t = Tmp::new();
    t.agent("echo");
    t.put("agents/echo/role/memory/m.md", "state");
    let ctx = t.ctx();
    let (dir, note) = resolved(&ctx, "echo");
    assert_eq!(note, "");
    assert!(dir.starts_with(&ctx.store), "{dir:?}");
    assert!(
        dir.file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("echo@")
    );
    assert_eq!(
        fs::read_to_string(dir.join("prompt.md")).unwrap(),
        "You are echo.\n\n# protocol\n"
    );
    assert_eq!(
        fs::read_to_string(dir.join("settings.json")).unwrap(),
        "{}\n"
    );
    assert!(!dir.join("memory").exists());
    assert_eq!(
        fs::read_to_string(dir.join(".home")).unwrap(),
        format!("{}\n", t.0.join("agents/echo").display())
    );
    let meta: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.join(".meta.json")).unwrap()).unwrap();
    assert_eq!(meta["agent"], "echo");
    assert!(meta["made"].is_f64());
    let good = fs::read_to_string(ctx.store.join(".pointers/echo.good")).unwrap();
    assert_eq!(
        good.trim(),
        dir.file_name()
            .unwrap()
            .to_string_lossy()
            .split_once('@')
            .unwrap()
            .1
    );
    // nothing changed: the same version
    assert_eq!(resolved(&ctx, "echo").0, dir);
    // an edit is a new version, and the old one stays (recent)
    t.put("agents/echo/role/notes.md", "more");
    let (next, note) = resolved(&ctx, "echo");
    assert_eq!(note, "");
    assert_ne!(next, dir);
    assert!(dir.is_dir());
}

#[test]
fn resolve_keeps_the_last_good_version_when_edits_fail_checks() {
    let t = Tmp::new();
    t.agent("echo");
    let ctx = t.ctx();
    let (good, _) = resolved(&ctx, "echo");
    t.put(
        "agents/echo/role/agent.md",
        "---\nname: echo\n---\nno description\n",
    );
    let (dir, note) = resolved(&ctx, "echo");
    assert_eq!(dir, good);
    let v = good
        .file_name()
        .unwrap()
        .to_string_lossy()
        .split_once('@')
        .unwrap()
        .1
        .to_owned();
    assert_eq!(
        note,
        format!("latest edits fail checks, running {v}: agent.md: frontmatter has no description")
    );
}

#[test]
fn resolve_with_no_good_version_runs_unchecked_edits() {
    let t = Tmp::new();
    t.put(
        "agents/odd/role/agent.md",
        "---\nname: odd\ndescription: d\neffort: extreme\nmodel: bad model!\n---\n{{NOPE}}\n",
    );
    let ctx = t.ctx();
    let (dir, note) = resolved(&ctx, "odd");
    assert_eq!(
        note,
        "no good version yet, running unchecked edits: agent.md: effort 'extreme' is not one of low, medium, high, xhigh, max; \
         agent.md: model 'bad model!' is not a model name or alias; agent.md: unknown placeholder(s) NOPE"
    );
    assert!(dir.join(".unchecked").is_file());
    // the second run knows it is the same unchecked version
    let (again, note) = resolved(&ctx, "odd");
    assert_eq!(
        (again, note.as_str()),
        (dir, "no good version yet, running unchecked edits")
    );
    assert!(!ctx.store.join(".pointers/odd.good").exists());
}

#[test]
fn help_is_answered_by_the_verb_not_clap() {
    let t = Tmp::new();
    let ctx = t.ctx();
    for h in ["-h", "--help"] {
        let out = cli::run(&ctx, &[h.to_owned()]);
        assert_eq!(out.code, 0);
        assert!(
            out.stdout.starts_with("usage: kith kernel pkg resolve"),
            "{}",
            out.stdout
        );
    }
}

#[test]
fn pins_and_editing_markers() {
    let t = Tmp::new();
    t.agent("echo");
    let ctx = t.ctx();
    let (dir, _) = resolved(&ctx, "echo");
    let v = dir
        .file_name()
        .unwrap()
        .to_string_lossy()
        .split_once('@')
        .unwrap()
        .1
        .to_owned();
    t.put("agents/echo/role/notes.md", "edit");
    t.put("agents/echo/role/.editing", "");
    assert_eq!(
        resolved(&ctx, "echo"),
        (dir.clone(), format!("being edited (.editing), running {v}"))
    );
    // an hour-old marker holds nothing back
    set_mtime(&t.0.join("agents/echo/role/.editing"), 1_577_836_800, 0);
    let (newer, note) = resolved(&ctx, "echo");
    assert_eq!(note, "");
    assert_ne!(newer, dir);
    fs::write(ctx.store.join(".pointers/echo.pin"), format!("{v}\n")).unwrap();
    assert_eq!(
        resolved(&ctx, "echo"),
        (
            dir,
            format!("pinned to {v} (kith unpin echo to follow edits)")
        )
    );
    // a pin to a version that is gone is ignored
    fs::write(ctx.store.join(".pointers/echo.pin"), "0123456789\n").unwrap();
    assert_eq!(resolved(&ctx, "echo").0, newer);
}

#[test]
fn gc_keeps_good_pinned_newest_recent_and_running() {
    let t = Tmp::new();
    t.agent("echo");
    let ctx = t.ctx();
    for (name, made) in [
        ("old1", 1.0e9),
        ("old2", 1.0e9 + 1.0),
        ("old3", 1.0e9 + 2.0),
        ("old4", 1.0e9 + 3.0),
    ] {
        t.put(
            &format!("state/store/echo@{name}/.meta.json"),
            &format!("{{\"made\": {made}}}"),
        );
    }
    // a live run (this test's own pid) holds old3; a dead one's record is cleared
    t.put(
        &format!("state/store/echo@old3/.runs/{}.live", std::process::id()),
        "",
    );
    t.put("state/store/echo@old1/.runs/999999.dead", "");
    resolved(&ctx, "echo");
    let left: Vec<String> = {
        let mut v: Vec<String> = fs::read_dir(&ctx.store)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("echo@old"))
            .collect();
        v.sort();
        v
    };
    // newest two are kept (the fresh one and old4), plus the running old3
    assert_eq!(left, vec!["echo@old3", "echo@old4"]);
}

#[test]
fn the_cli_prints_what_the_python_printed() {
    let t = Tmp::new();
    t.agent("echo");
    t.agent("alpha");
    let ctx = t.ctx();
    let args = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    let out = cli::run(&ctx, &args(&["resolve"]));
    assert_eq!(
        (out.code, out.stderr.as_str()),
        (2, "kith-pkg: resolve <agent>|--all\n")
    );
    let out = cli::run(&ctx, &args(&["resolve", "ghost", "--json"]));
    assert_eq!((out.code, out.stdout.as_str()), (1, ""));
    assert_eq!(out.stderr, "kith-pkg: no agent 'ghost' (kith-pkg list)\n");
    let out = cli::run(&ctx, &args(&["resolve", "--all", "extra"]));
    assert_eq!(out.stderr, "kith-pkg: no agent '--all' (kith-pkg list)\n");
    let out = cli::run(&ctx, &args(&["resolve", "--all"]));
    assert_eq!(out.code, 0);
    let dirs: Vec<&str> = out.stdout.lines().collect();
    assert_eq!(dirs.len(), 2);
    assert!(
        dirs[0].contains("/alpha@") && dirs[1].contains("/echo@"),
        "{dirs:?}"
    );
    let out = cli::run(&ctx, &args(&["resolve", "echo", "--json"]));
    let v: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    assert_eq!(v["echo"]["note"], "");
    assert_eq!(v["echo"]["dir"], dirs[1]);
    assert_eq!(v["echo"]["version"].as_str().unwrap().len(), 10);
    // key order and separators are json.dumps's
    assert!(
        out.stdout.starts_with("{\"echo\": {\"dir\": \""),
        "{}",
        out.stdout
    );
    let out = cli::run(&ctx, &args(&["versions", "echo"]));
    assert_eq!(out.code, 2);
}

#[test]
fn a_snapshot_that_throws_runs_the_live_folder() {
    let t = Tmp::new();
    t.agent("echo");
    std::os::unix::fs::symlink("nowhere.md", t.0.join("agents/echo/role/gone.md")).unwrap();
    let ctx = t.ctx();
    let out = cli::run(
        &ctx,
        &["resolve".to_owned(), "echo".to_owned(), "--json".to_owned()],
    );
    let v: serde_json::Value = serde_json::from_str(&out.stdout).unwrap();
    let live = t.0.join("agents/echo/role");
    assert_eq!(v["echo"]["dir"], live.to_string_lossy().as_ref());
    assert_eq!(v["echo"]["version"], "role");
    assert_eq!(
        v["echo"]["note"],
        format!(
            "snapshot failed ([Errno 2] No such file or directory: '{}'); running the live folder",
            live.join("gone.md").display()
        )
    );
}
