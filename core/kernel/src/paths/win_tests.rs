//! The Windows forms of the path rules. The string helpers are pure and run everywhere; the filesystem ones run in the
//! Windows CI job.

#![allow(clippy::unwrap_used)]

use super::*;

fn os(s: &str) -> Option<OsString> {
    Some(OsString::from(s))
}

#[test]
fn home_prefers_home_then_userprofile_on_windows_only() {
    assert_eq!(home_from(os("/h"), os("C:\\Users\\k"), true), PathBuf::from("/h"));
    assert_eq!(home_from(None, os("C:\\Users\\k"), true), PathBuf::from("C:\\Users\\k"));
    assert_eq!(home_from(os(""), os("C:\\Users\\k"), true), PathBuf::from("C:\\Users\\k"));
    assert_eq!(home_from(None, os("C:\\Users\\k"), false), PathBuf::new());
    assert_eq!(home_from(None, None, true), PathBuf::new());
    assert_eq!(home_from(None, os("Users\\k"), true), PathBuf::new(), "a relative profile is refused");
    assert_eq!(home_from(None, os("C:/Users/k"), true), PathBuf::from("C:/Users/k"));
}

#[test]
fn tilde_takes_either_slash_on_windows_only() {
    assert!(tilde_rest("/a", false) && tilde_rest("/a", true));
    assert!(tilde_rest("\\a", true) && !tilde_rest("\\a", false));
    assert!(!tilde_rest("name", true), "another user's ~name stays as written");
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
    assert_eq!(expanduser(home, r"~\agents"), PathBuf::from(r"C:\Users\k\agents"));
    assert_eq!(expanduser(home, "~/agents"), PathBuf::from(r"C:\Users\k\agents"));
    assert_eq!(expanduser(home, "~"), PathBuf::from(r"C:\Users\k"));
}
