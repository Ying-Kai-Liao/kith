//! The Windows forms of the path rules. The string helpers are pure and run everywhere; the filesystem ones run in the
//! Windows CI job.

#![allow(clippy::unwrap_used)]

use super::*;

#[allow(clippy::unnecessary_wraps)] // the Option is what `home_from` takes
fn os(s: &str) -> Option<OsString> {
    Some(OsString::from(s))
}

#[test]
fn home_prefers_home_then_userprofile_on_windows_only() {
    assert_eq!(
        home_from(os("/h"), os("C:\\Users\\k"), true),
        PathBuf::from("/h")
    );
    assert_eq!(
        home_from(None, os("C:\\Users\\k"), true),
        PathBuf::from("C:\\Users\\k")
    );
    assert_eq!(
        home_from(os(""), os("C:\\Users\\k"), true),
        PathBuf::from("C:\\Users\\k")
    );
    assert_eq!(home_from(None, os("C:\\Users\\k"), false), PathBuf::new());
    assert_eq!(home_from(None, None, true), PathBuf::new());
    assert_eq!(
        home_from(None, os("Users\\k"), true),
        PathBuf::new(),
        "a relative profile is refused"
    );
    assert_eq!(
        home_from(None, os("C:/Users/k"), true),
        PathBuf::from("C:/Users/k")
    );
}

#[test]
fn tilde_takes_either_slash_on_windows_only() {
    assert!(tilde_rest("/a", false) && tilde_rest("/a", true));
    assert!(tilde_rest("\\a", true) && !tilde_rest("\\a", false));
    assert!(
        !tilde_rest("name", true),
        "another user's ~name stays as written"
    );
}

#[test]
fn verbatim_prefix_is_stripped() {
    assert_eq!(strip_verbatim(r"\\?\C:\Users\k"), r"C:\Users\k");
    assert_eq!(strip_verbatim(r"\\?\UNC\srv\share\x"), r"\\srv\share\x");
    assert_eq!(strip_verbatim(r"C:\Users\k"), r"C:\Users\k");
    assert_eq!(strip_verbatim(r"\\?\Volume{1}\x"), r"\\?\Volume{1}\x");
    assert_eq!(strip_verbatim("/home/k"), "/home/k");
}

#[cfg(windows)]
#[test]
fn windows_forms_resolve() {
    let canon = realpath(&std::env::temp_dir());
    assert!(!canon.to_string_lossy().starts_with(r"\\?\"), "{canon:?}");
    assert!(canon.is_absolute());
    assert_eq!(realpath(Path::new("C:/")), PathBuf::from(r"C:\"));
    assert_eq!(realpath(Path::new(r"C:\")), PathBuf::from(r"C:\"));
    assert_eq!(realpath(Path::new(r"C:\a\..\b")), PathBuf::from(r"C:\b"));
    let home = Path::new(r"C:\Users\k");
    assert_eq!(
        expanduser(home, r"~\agents"),
        PathBuf::from(r"C:\Users\k\agents")
    );
    assert_eq!(
        expanduser(home, "~/agents"),
        PathBuf::from(r"C:\Users\k\agents")
    );
    assert_eq!(expanduser(home, "~"), PathBuf::from(r"C:\Users\k"));
}

#[cfg(windows)]
fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("kith-which-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

// `which_with(.., windows = true)` runs on every OS, but the files it needs only count as runnable without mode bits on Windows.
#[cfg(windows)]
#[test]
fn which_tries_pathext_in_order_and_skips_the_bare_shim() {
    let d = scratch("ext");
    for f in ["claude", "claude.cmd", "claude.ps1", "tool.exe"] {
        std::fs::write(d.join(f), b"").unwrap();
    }
    let path = std::env::join_paths([&d]).unwrap();
    let w = |p: &str, ext: Option<&str>| which_with(p, &path, ext, true);
    // the extensionless sh shim is never picked: .cmd wins
    assert_eq!(w("claude", None), Some(d.join("claude.cmd")));
    assert_eq!(w("claude", Some(".exe;.CMD")), Some(d.join("claude.cmd")));
    assert_eq!(w("tool", Some(".cmd;.exe")), Some(d.join("tool.exe")));
    // a name with an extension is tried as is
    assert_eq!(w("tool.exe", None), Some(d.join("tool.exe")));
    assert_eq!(w("claude.ps1", None), Some(d.join("claude.ps1")));
    assert_eq!(w("nothing", None), None);
    std::fs::remove_dir_all(&d).unwrap();
}

#[cfg(windows)]
#[test]
fn which_strips_quotes_and_ignores_empty_path_entries() {
    let d = scratch("q x");
    std::fs::write(d.join("a.exe"), b"").unwrap();
    let path = OsString::from(format!(";\"{}\"", d.display()));
    assert_eq!(which_with("a", &path, None, true), Some(d.join("a.exe")));
    std::fs::remove_dir_all(&d).unwrap();
}
