#![allow(clippy::unwrap_used)]
//! The unit rules of the room-ledger hook; the whole verb is compared with the Python by `kernel/tests/difftest`
//! (golden/room-ledger.json). The exact bytes of the ledger are held here: the difftest reads `*.json` as data.

use std::path::{Path, PathBuf};

use super::{Ctx, is_id, py_strip, run};

fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("kith-roomledger-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("home/memory/rooms/grp1")).unwrap();
    // realpath without the `\\?\` prefix Windows puts on it, which the ledger's own `realpath` does not keep
    PathBuf::from(crate::paths::strip_verbatim(
        &std::fs::canonicalize(&d).unwrap().to_string_lossy(),
    ))
}

fn ctx(root: &Path, in_room: &str, members: &str) -> Ctx {
    Ctx {
        state: root.join("state"),
        room: in_room.into(),
        members: members.into(),
    }
}

fn hook(c: &Ctx, root: &Path, mode: &str, file: &Path) {
    let home = root.join("home");
    let args: Vec<String> = [mode, "--agent", "echo", "--home", home.to_str().unwrap()]
        .map(String::from)
        .to_vec();
    let input = format!(
        "{{\"tool_input\": {{\"file_path\": {:?}}}}}",
        file.to_str().unwrap()
    );
    assert_eq!(run(c, &args, &mut input.as_bytes()), 0);
}

fn ledger(root: &Path) -> String {
    std::fs::read_to_string(root.join("state/room-ledger/echo/grp1.json")).unwrap()
}

#[test]
fn a_first_write_records_who_was_there_sorted_and_a_later_one_narrows() {
    let root = scratch("narrow");
    let note = root.join("home/memory/rooms/grp1/a.md");
    hook(
        &ctx(&root, "grp1", " Sam , kai,BAD id,sam"),
        &root,
        "post",
        &note,
    );
    assert_eq!(
        ledger(&root),
        "{\n \"rooms/grp1/a.md\": [\n  \"kai\",\n  \"sam\"\n ]\n}"
    );
    hook(&ctx(&root, "grp1", "sam"), &root, "post", &note);
    assert_eq!(
        ledger(&root),
        "{\n \"rooms/grp1/a.md\": [\n  \"sam\"\n ]\n}"
    );
    // another room's run, and a run with nobody named, narrow it to the owner alone
    hook(&ctx(&root, "grp2", "sam"), &root, "post", &note);
    assert_eq!(ledger(&root), "{\n \"rooms/grp1/a.md\": []\n}");
    assert!(root.join("state/room-ledger/echo/grp1.json.lock").is_file());
}

#[test]
fn pre_gives_an_unrecorded_existing_note_to_the_owner_and_a_missing_one_nothing() {
    let root = scratch("pre");
    let (old, new) = (
        root.join("home/memory/rooms/grp1/old.md"),
        root.join("home/memory/rooms/grp1/new.md"),
    );
    std::fs::write(&old, "x").unwrap();
    let c = ctx(&root, "grp1", "sam");
    hook(&c, &root, "pre", &new);
    assert!(!root.join("state/room-ledger").exists());
    hook(&c, &root, "pre", &old);
    hook(&c, &root, "pre", &old);
    hook(&c, &root, "post", &old);
    assert_eq!(ledger(&root), "{\n \"rooms/grp1/old.md\": []\n}");
    hook(&c, &root, "post", &new);
    assert!(ledger(&root).contains("\"rooms/grp1/new.md\": [\n  \"sam\"\n ]"));
}

#[test]
fn a_ledger_that_is_not_an_object_is_left_alone_and_a_broken_one_starts_over() {
    let root = scratch("odd");
    let note = root.join("home/memory/rooms/grp1/a.md");
    let f = root.join("state/room-ledger/echo/grp1.json");
    std::fs::create_dir_all(f.parent().unwrap().parent().unwrap()).unwrap();
    // made as the hook makes it: a folder others can open is refused on Windows
    crate::ownerfs::create_dir(f.parent().unwrap()).unwrap();
    std::fs::write(&f, "[\"x\"]").unwrap();
    hook(&ctx(&root, "grp1", "sam"), &root, "post", &note);
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "[\"x\"]");
    std::fs::write(&f, "{oops").unwrap();
    hook(&ctx(&root, "grp1", "sam"), &root, "post", &note);
    assert!(ledger(&root).contains("\"sam\""));
}

#[test]
fn ids_strip_and_lowercase_as_python_does() {
    assert!(is_id("a") && is_id("a.b_c-d") && is_id("a\n") && is_id(&"x".repeat(64)));
    assert!(
        !is_id("")
            && !is_id(".a")
            && !is_id(&"x".repeat(65))
            && !is_id("a b")
            && !is_id("a\n\n")
            && !is_id("é")
    );
    assert_eq!(py_strip("\x1c a \u{a0}"), "a");
}

#[test]
fn a_path_with_a_nul_is_never_recorded() {
    let root = scratch("nul");
    let c = ctx(&root, "grp1", "sam");
    let home = root.join("home");
    let args: Vec<String> = ["post", "--agent", "echo", "--home", home.to_str().unwrap()]
        .map(String::from)
        .to_vec();
    // a real \u0000 in the JSON, as the difftest case sends it
    let input = format!(
        "{{\"tool_input\": {{\"file_path\": \"{}/memory/rooms/grp1/a\\u0000b.md\"}}}}",
        home.to_str().unwrap().replace('\\', "\\\\")
    );
    assert_eq!(run(&c, &args, &mut input.as_bytes()), 0);
    assert!(!root.join("state/room-ledger/echo/grp1.json").exists());
}

#[test]
fn two_handles_on_the_ledger_lock_take_turns() {
    use fs2::FileExt as _;
    let root = scratch("lock");
    let file = root.join("state/room-ledger/echo/grp1.json");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    let first = super::lock(&file).unwrap();
    // another handle on the same file is turned away while the first holds it, and gets in once it is dropped
    let name = root.join("state/room-ledger/echo/grp1.json.lock");
    let other = std::fs::OpenOptions::new().write(true).open(name).unwrap();
    assert!(other.try_lock_exclusive().is_err());
    drop(first);
    // the lock goes with the handle: a new one gets in at once
    drop(other);
    drop(super::lock(&file).unwrap());
}

/// A ledger folder made before, with the default list, is not written to: the note stays unrecorded (owner only).
#[cfg(windows)]
#[test]
fn a_ledger_folder_open_to_others_is_refused() {
    let root = scratch("open");
    let note = root.join("home/memory/rooms/grp1/a.md");
    let dir = root.join("state/room-ledger/echo");
    std::fs::create_dir_all(&dir).unwrap();
    // Everyone (S-1-1-0), by SID so the language of the system does not matter
    let st = std::process::Command::new("icacls")
        .arg(&dir)
        .args(["/grant", "*S-1-1-0:(OI)(CI)R"])
        .status()
        .unwrap();
    assert!(st.success());
    assert!(crate::ownerfs::check_dir(&dir).is_err());
    hook(&ctx(&root, "grp1", "sam"), &root, "post", &note);
    assert!(!root.join("state/room-ledger/echo/grp1.json").exists());
}

/// A folder the hook makes is owner-only.
#[cfg(windows)]
#[test]
fn a_ledger_folder_the_hook_makes_is_owner_only() {
    let root = scratch("mine");
    let note = root.join("home/memory/rooms/grp1/a.md");
    hook(&ctx(&root, "grp1", "sam"), &root, "post", &note);
    let dir = root.join("state/room-ledger/echo");
    assert_eq!(
        kith_winsys::acl::check(&dir).unwrap(),
        kith_winsys::acl::Check::OwnerOnly
    );
    assert!(root.join("state/room-ledger/echo/grp1.json").is_file());
}
