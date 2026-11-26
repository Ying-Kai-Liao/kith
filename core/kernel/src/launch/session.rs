//! The session of a run here: the id it is claimed under, the conversation lock's launcher that starts Claude, and the caller's
//! extra Claude arguments (`kernel/bin/kith`, "Every Claude start holds its session's writer claim").

use std::path::{Path, PathBuf};

use super::assemble::{Env, get, random_hex};

/// A random (version 4) UUID, in the text form `uuid.uuid4()` prints: from the OS's random source, never a clock.
pub fn uuid_v4() -> Result<String, String> {
    let hex = random_hex()?;
    let mut b: Vec<u8> = hex.bytes().collect();
    // The version nibble is 4 and the variant's two high bits are 10.
    b[12] = b'4';
    let variant = (b[16] as char).to_digit(16).unwrap_or(0);
    b[16] = char::from_digit((variant & 0x3) | 0x8, 16).map_or(b'8', |c| c as u8);
    let h = String::from_utf8(b).map_err(|e| e.to_string())?;
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    ))
}

/// The session id the run holds its claim under, from Claude's arguments: the one it was told to be (`--session-id`), else the
/// one it continues (`--resume`, not a fork), else a fresh one given now so there is one to claim. A bare `--continue` names
/// none up front (`""`: no caller passes one; kith's own `-c` is a `--resume` by now).
///
/// The second answer says the id was made here and goes into the arguments (`--session-id ID`).
pub fn session_of(args: &[String]) -> Result<(String, bool), String> {
    let (mut given, mut resumed) = (String::new(), String::new());
    let (mut fork, mut cont) = (false, false);
    let mut prev = "";
    for a in args {
        match prev {
            "--resume" => resumed.clone_from(a),
            "--session-id" => given.clone_from(a),
            _ => {}
        }
        if let Some(v) = a.strip_prefix("--resume=") {
            resumed = v.to_owned();
        } else if let Some(v) = a.strip_prefix("--session-id=") {
            given = v.to_owned();
        } else if a == "--fork-session" {
            fork = true;
        } else if a == "--continue" || a == "-c" {
            cont = true;
        }
        prev = a;
    }
    if !given.is_empty() {
        Ok((given, false))
    } else if !resumed.is_empty() && !fork {
        Ok((resumed, false))
    } else if !cont || fork {
        Ok((uuid_v4()?, true))
    } else {
        Ok((String::new(), false))
    }
}

/// The stub takes keys of a-z 0-9 . _ - only (exit 70 otherwise, which after exec could not be told from a lock error).
pub fn lockable(id: &str) -> bool {
    !id.is_empty()
        && id.bytes().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'_' | b'-')
        })
}

/// How the run starts under the conversation's writer lock.
#[derive(Debug, PartialEq, Eq)]
pub enum Locked {
    /// Under `kith-stub`: its path and the lock folder.
    Stub(PathBuf),
    /// Unlocked, with the one stderr line that says why (or none for a run with no session id).
    Unlocked(Option<String>),
    /// Refused (`KITH_REQUIRE_STUB=1`): the line, and the run does not start.
    Refused(String),
}

/// The program that takes the lock for session `csid`: `KITH_STUB_BIN`, else beside the kith program where it really lives
/// (`install.sh` links only `kith` into `~/.local/bin`, so the stub sits next to the link's target), else beside the path as given.
/// A missing stub is a warning, not a refusal: an owner's run must just work, and a dev checkout or an older install may have none.
pub fn lock_for(csid: &str, env: &Env, kbin: Option<&Path>) -> Locked {
    if csid.is_empty() {
        return Locked::Unlocked(None);
    }
    let require = get(env, "KITH_REQUIRE_STUB") == "1";
    if !lockable(csid) {
        let why =
            format!("session id \"{csid}\" cannot be locked (only a-z 0-9 . _ - are allowed)");
        return if require {
            Locked::Refused(format!("kith: {why}"))
        } else {
            Locked::Unlocked(Some(format!("kith: {why}; this run is not locked")))
        };
    }
    let given = get(env, "KITH_STUB_BIN");
    let found = if given.is_empty() {
        kbin.and_then(|k| {
            crate::helpers::find_stub(Some(k), Some(k)).or_else(|| {
                let name = if cfg!(windows) {
                    "kith-stub.exe"
                } else {
                    "kith-stub"
                };
                k.parent()
                    .map(|d| d.join(name))
                    .filter(|p| crate::helpers::executable(p))
            })
        })
    } else {
        Some(PathBuf::from(given))
    };
    match found.filter(|p| crate::helpers::executable(p)) {
        Some(p) => Locked::Stub(p),
        None if require => Locked::Refused(
            "kith: no kith-stub program beside kith, so this conversation cannot be locked; install the current kith (kith update self)"
                .to_owned(),
        ),
        None => Locked::Unlocked(Some(
            "kith: no kith-stub program beside kith; this run is not locked against another run of the same conversation".to_owned(),
        )),
    }
}

/// The stub's command line: `STUB [--lock-alone] --lock-dir DIR --key ID --`.
pub fn stub_argv(stub: &Path, machine: &str, csid: &str, alone: bool) -> Vec<String> {
    let mut v = vec![stub.to_string_lossy().into_owned()];
    if alone {
        v.push("--lock-alone".to_owned());
    }
    v.extend([
        "--lock-dir".to_owned(),
        Path::new(machine)
            .join("core/locks")
            .to_string_lossy()
            .into_owned(),
        "--key".to_owned(),
        csid.to_owned(),
        "--".to_owned(),
    ]);
    v
}

/// The caller's `KITH_CLAUDE_ARGS` (e.g. `--output-format json --resume <session>` from `kith serve`), split on blanks. For
/// anyone but the owner the permission flags in them are dropped: the membership's mode stands.
pub fn extra_args(raw: &str, owner: bool) -> Vec<String> {
    // `read -r -a` takes the first line.
    let words = raw.lines().next().unwrap_or("").split_whitespace();
    if owner {
        return words.map(str::to_owned).collect();
    }
    let mut kept = vec![];
    let mut skip = false;
    for a in words {
        if skip {
            skip = false;
            continue;
        }
        match a {
            "--permission-mode" => skip = true,
            "--dangerously-skip-permissions" | "--allow-dangerously-skip-permissions" => {}
            _ if a.starts_with("--permission-mode=") => {}
            _ => kept.push(a.to_owned()),
        }
    }
    kept
}

/// `export NAME=VALUE` lines as `kith-permit issue` prints them (the value shell-quoted): the names and values.
pub fn exports(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let (name, value) = l.strip_prefix("export ")?.split_once('=')?;
            Some((name.to_owned(), unquote(value)))
        })
        .collect()
}

/// What `shlex.quote` made of a word: bare, or in single quotes with `'"'"'` for a quote inside.
fn unquote(v: &str) -> String {
    let mut out = String::new();
    let mut chars = v.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                for q in chars.by_ref() {
                    if q == '\'' {
                        break;
                    }
                    out.push(q);
                }
            }
            '"' => {
                for q in chars.by_ref() {
                    if q == '"' {
                        break;
                    }
                    out.push(q);
                }
            }
            '\\' => out.extend(chars.next()),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn v(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn a_uuid_is_version_4_and_never_the_same_twice() {
        let (a, b) = (uuid_v4().unwrap(), uuid_v4().unwrap());
        assert_ne!(a, b);
        let parts: Vec<&str> = a.split('-').collect();
        assert_eq!(
            parts.iter().map(|p| p.len()).collect::<Vec<_>>(),
            [8, 4, 4, 4, 12]
        );
        assert!(parts[2].starts_with('4'));
        assert!(matches!(parts[3].as_bytes()[0], b'8' | b'9' | b'a' | b'b'));
        assert!(lockable(&a));
    }

    #[test]
    fn the_session_is_the_given_one_the_resumed_one_or_a_new_one() {
        assert_eq!(
            session_of(&v(&["--session-id", "abc", "--resume", "x"])).unwrap(),
            ("abc".to_owned(), false)
        );
        assert_eq!(
            session_of(&v(&["--resume", "s1"])).unwrap(),
            ("s1".to_owned(), false)
        );
        assert_eq!(
            session_of(&v(&["--resume=s2"])).unwrap(),
            ("s2".to_owned(), false)
        );
        let (fresh, made) = session_of(&v(&["--resume", "s1", "--fork-session"])).unwrap();
        assert!(made && fresh != "s1", "a fork is a new session");
        let (fresh, made) = session_of(&v(&["--model", "x"])).unwrap();
        assert!(made && lockable(&fresh));
        assert_eq!(
            session_of(&v(&["--continue"])).unwrap(),
            (String::new(), false),
            "a bare continue names none"
        );
    }

    #[test]
    fn only_plain_keys_can_be_locked() {
        assert!(lockable("a-b.c_9"));
        assert!(!lockable("A"));
        assert!(!lockable("a/b"));
        assert!(!lockable(""));
    }

    #[test]
    fn a_guest_loses_the_permission_flags_of_the_callers_arguments() {
        let raw = "--output-format json --permission-mode bypassPermissions --dangerously-skip-permissions --permission-mode=x --resume s";
        assert_eq!(
            extra_args(raw, false),
            v(&["--output-format", "json", "--resume", "s"])
        );
        assert_eq!(extra_args(raw, true).len(), 8);
        assert_eq!(extra_args("a b\nc", true), v(&["a", "b"]));
    }

    #[test]
    fn permit_exports_read_back() {
        let t = "export KITH_ASK_TOKEN='a b'\nexport KITH_ASK_SOCKET=/x/y.sock\nexport Q='it'\"'\"'s'\n";
        assert_eq!(
            exports(t),
            vec![
                ("KITH_ASK_TOKEN".to_owned(), "a b".to_owned()),
                ("KITH_ASK_SOCKET".to_owned(), "/x/y.sock".to_owned()),
                ("Q".to_owned(), "it's".to_owned()),
            ]
        );
    }

    #[test]
    fn the_stub_line_names_the_lock_folder_and_the_key() {
        // The folder is joined with the OS's separator.
        let locks = Path::new("/m").join("core/locks");
        let locks = locks.to_string_lossy();
        assert_eq!(
            stub_argv(Path::new("/b/kith-stub"), "/m", "sid", true),
            v(&[
                "/b/kith-stub",
                "--lock-alone",
                "--lock-dir",
                &locks,
                "--key",
                "sid",
                "--"
            ])
        );
    }

    #[test]
    fn a_missing_stub_warns_and_the_required_one_refuses() {
        let mut env = Env::new();
        env.insert(
            "KITH_STUB_BIN".to_owned(),
            "/nonexistent/kith-stub".to_owned(),
        );
        assert!(matches!(
            lock_for("sid", &env, None),
            Locked::Unlocked(Some(_))
        ));
        assert_eq!(lock_for("", &env, None), Locked::Unlocked(None));
        env.insert("KITH_REQUIRE_STUB".to_owned(), "1".to_owned());
        assert!(matches!(lock_for("sid", &env, None), Locked::Refused(_)));
        assert!(matches!(lock_for("BAD ID", &env, None), Locked::Refused(_)));
    }
}
