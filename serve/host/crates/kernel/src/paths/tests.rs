//! The layouts of `kernel/lib/kith_paths.py`, each in a throwaway home with an injected environment.
//! `kernel/tests/kith-paths-parity-check` runs the same layouts through both implementations.

use super::*;
use std::os::unix::fs::symlink;
use std::sync::atomic::{AtomicU32, Ordering};

struct Home(PathBuf);

impl Home {
    fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "kith-kernel-paths-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // The tempdir itself may sit behind a link (macOS /var): compare real paths everywhere.
        Self(realpath(&dir))
    }
    fn p(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }
    fn dir(&self, rel: &str) -> &Self {
        std::fs::create_dir_all(self.p(rel)).unwrap();
        self
    }
    fn link(&self, rel: &str, to: &str) -> &Self {
        symlink(self.p(to), self.p(rel)).unwrap();
        self
    }
    fn paths(&self, env: &[(&str, &str)]) -> Paths {
        let env: Vec<(String, String)> = env.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect();
        let lookup = move |name: &str| env.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
        paths(&self.0, &self.p("kernel-elsewhere/kernel"), &lookup)
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn marker_wins_over_a_real_old_folder() {
    let h = Home::new();
    h.dir(".hq").dir(".kith");
    std::fs::write(h.p(".kith/.migrated"), "").unwrap();
    let p = h.paths(&[]);
    assert_eq!(p.state, h.p(".kith"));
    assert_eq!(p.machine, h.p(".kith"));
    assert_eq!(p.secrets, h.p(".kith/secrets"));
    assert_eq!(p.logs, h.p(".kith/logs"));
    assert_eq!(p.cache, h.p(".kith/cache"));
    assert_eq!(p.store, h.p(".kith/store"));
}

#[test]
fn a_real_old_folder_is_this_macs_until_migrated() {
    let h = Home::new();
    h.dir(".hq");
    assert_eq!(h.paths(&[]).state, h.p(".hq"));
    // kith serve already made ~/.kith/serve: still not migrated.
    h.dir(".kith/serve");
    assert_eq!(h.paths(&[]).state, h.p(".hq"));
    assert_eq!(h.paths(&[]).secrets, h.p(".hq/secrets"));
}

#[test]
fn old_folder_linked_to_the_new_one() {
    let h = Home::new();
    h.dir(".kith").link(".hq", ".kith");
    assert_eq!(state_dir(&h.0, &|_| None), (h.p(".kith"), true));
}

#[test]
fn old_folder_linked_elsewhere_is_the_old_link_layout() {
    let h = Home::new();
    h.dir("elsewhere/kernel").dir("elsewhere/agents").link(".hq", "elsewhere").link("kith", "elsewhere");
    let p = h.paths(&[]);
    assert_eq!(state_dir(&h.0, &|_| None), (h.p(".hq"), false));
    assert_eq!(p.home, h.p("kith"));
    assert_eq!(p.state, h.p("kith/state"));
    assert_eq!(p.home_real, h.p("elsewhere"));
    assert_eq!(p.machine, h.p(".config/kith"));
    assert_eq!(p.secrets, h.p(".config/kith"));
    assert_eq!(p.logs, h.p("Library/Logs/kith"));
    assert_eq!(p.cache, h.p("Library/Caches/kith"));
    assert_eq!(p.store, h.p("kith/agents/.store"));
    // The hq-named folders still win where they exist.
    h.dir(".config/hq").dir("Library/Logs/hq");
    let p = h.paths(&[]);
    assert_eq!(p.machine, h.p(".config/hq"));
    assert_eq!(p.logs, h.p("Library/Logs/hq"));
    assert_eq!(p.cache, h.p("Library/Caches/kith"));
}

#[test]
fn only_the_new_folder() {
    let h = Home::new();
    h.dir(".kith");
    assert_eq!(state_dir(&h.0, &|_| None), (h.p(".kith"), true));
}

#[test]
fn nothing_at_all_is_the_old_link_layout() {
    let h = Home::new();
    let p = h.paths(&[]);
    assert_eq!(state_dir(&h.0, &|_| None), (h.p(".kith"), false));
    // No ~/kith, no ~/hq: the folder around the kernel.
    assert_eq!(p.home, h.p("kernel-elsewhere"));
    assert_eq!(p.state, h.p("kernel-elsewhere/state"));
    assert_eq!(p.machine, h.p(".config/kith"));
}

#[test]
fn a_kernel_not_called_kernel_is_its_own_home() {
    let h = Home::new();
    let p = paths(&h.0, Path::new("/opt/checkout"), &|_| None);
    assert_eq!(p.home, Path::new("/opt/checkout"));
}

#[test]
fn state_given_wins_and_old_names_work() {
    let h = Home::new();
    h.dir(".hq");
    let p = h.paths(&[("KITH_STATE", "/somewhere/else")]);
    assert_eq!((p.state, p.machine), (PathBuf::from("/somewhere/else"), PathBuf::from("/somewhere/else")));
    assert_eq!(h.paths(&[("HQ_STATE", "/old/name")]).state, PathBuf::from("/old/name"));
    // KITH_ beats HQ_; an empty KITH_ counts as unset; blanks are trimmed away.
    assert_eq!(h.paths(&[("KITH_STATE", "/new"), ("HQ_STATE", "/old")]).state, PathBuf::from("/new"));
    assert_eq!(h.paths(&[("KITH_STATE", ""), ("HQ_STATE", "/old")]).state, PathBuf::from("/old"));
    assert_eq!(h.paths(&[("KITH_STATE", "  /padded  ")]).state, PathBuf::from("/padded"));
    assert_eq!(h.paths(&[("KITH_STATE", "   ")]).state, h.p(".hq"));
    // A leading ~ is the home folder.
    assert_eq!(h.paths(&[("KITH_STATE", "~/s")]).state, h.p("s"));
}

#[test]
fn state_in_a_home_with_a_kernel_is_the_old_export() {
    let h = Home::new();
    h.dir("linked/kernel").link("kith", "linked");
    let p = h.paths(&[("KITH_STATE", h.p("linked/state").to_str().unwrap())]);
    assert_eq!(p.state, h.p("kith/state"));
    assert_eq!(p.machine, h.p(".config/kith"));
    // Without a kernel next to it, the same name is a plain choice.
    let h = Home::new();
    let p = h.paths(&[("KITH_STATE", h.p("other/state").to_str().unwrap())]);
    assert_eq!(p.state, h.p("other/state"));
    assert_eq!(p.secrets, h.p("other/state/secrets"));
}

#[test]
fn kith_home_given_and_the_links() {
    let h = Home::new();
    h.dir(".kith");
    let given = h.p("chosen");
    let p = h.paths(&[("KITH_HOME", given.to_str().unwrap())]);
    assert_eq!((p.home, p.agents), (given.clone(), given.join("agents")));
    // Even a home that does not exist: the real path keeps what is missing.
    assert_eq!(h.paths(&[("HQ_HOME", "~/nope/../chosen")]).home_real, given);
    // ~/kith beats ~/hq.
    h.dir("hq");
    assert_eq!(h.paths(&[]).home, h.p("hq"));
    h.dir("kith");
    assert_eq!(h.paths(&[]).home, h.p("kith"));
}

#[test]
fn links_to_the_kith_folder() {
    let h = Home::new();
    h.dir("disk/kernel").link("kith", "disk").link("hq", "kith");
    let p = h.paths(&[]);
    assert_eq!(p.home, h.p("kith"));
    assert_eq!(p.agents_real, h.p("disk/agents"));
    let h = Home::new();
    h.dir("disk").link("hq", "disk");
    let p = h.paths(&[]);
    assert_eq!((p.home.clone(), p.home_real), (h.p("hq"), h.p("disk")));
}

#[test]
fn the_new_folder_as_a_link_is_the_kith_folder_when_nothing_else_is() {
    let h = Home::new();
    h.dir("disk").link(".kith", "disk");
    let p = h.paths(&[]);
    assert_eq!(p.home, h.p(".kith"));
}

#[test]
fn realpath_resolves_what_exists_and_keeps_the_rest() {
    let h = Home::new();
    h.dir("a/b").link("l", "a");
    assert_eq!(realpath(&h.p("l/b/missing/../x")), h.p("a/b/x"));
    assert_eq!(realpath(&h.p("l/b/./c")), h.p("a/b/c"));
    // A loop is left as it is.
    symlink(h.p("loop2"), h.p("loop1")).unwrap();
    symlink(h.p("loop1"), h.p("loop2")).unwrap();
    assert!(realpath(&h.p("loop1")).starts_with(&h.0));
}

#[test]
fn text_formats() {
    let h = Home::new();
    h.dir(".kith");
    let p = h.paths(&[]);
    assert!(p.json().starts_with("{\"KITH_HOME\": \""));
    assert!(p.json().contains("\"HQ_STORE\": "));
    assert_eq!(p.entries().len(), 22);
    assert!(p.text().starts_with("KITH_HOME       "));
    assert_eq!(shell_quote("a b"), "'a b'");
    assert_eq!(shell_quote("/a/b-c"), "/a/b-c");
    assert_eq!(quote("é\"\\"), "\"\\u00e9\\\"\\\\\"");
}
