//! `kith account add|login|remove`, as `kernel/bin/kith-account` runs them (`cmd_add`, `cmd_login`, `cmd_remove`).
//!
//! `add ... --json` makes the folder and returns the login command for the caller to run in a terminal; it runs nothing.
//! Without `--json` (a person at a terminal, or the host's pty) the harness's own login runs here, in the account's
//! folder, as the person: kith never reads, copies or prints a token. What a verb prints before it runs the login or asks
//! a question is written and flushed at once, so the person sees it first.

use super::edits::{self, edit_with};
use super::set_verbs::not_ready_note;
use super::verbs::{Verb, flag, valid_id};
use super::{Ctx, J, Map, mirror, settings};
use std::io::{BufRead as _, IsTerminal as _, Write as _};
use std::path::Path;

const WARNING: &str = "\
Before adding a second Claude subscription account:

  Anthropic's usage policy does not allow using several accounts to get past usage limits. Accounts used that
  way can be suspended without notice. Whether to do this is your decision; kith only keeps the logins apart.

  Allowed ways to get more room: a Claude API key (kith account add claude-api:<profile>) or another provider
  (kith account add codex <name>).
";

/// Writes at once, so a prompt or a note shows before the login or the answer.
fn say(text: &str) {
    let mut out = std::io::stdout();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

/// One line from the person; nothing at the end of the input.
fn ask(prompt: &str) -> String {
    say(prompt);
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    line.trim().to_lowercase()
}

fn name_ok(n: &str) -> bool {
    let mut c = n.chars();
    c.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && c.all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
}

/// `harness_name`: `claude|codex <name>`.
fn harness_name(args: &[String], usage: &str) -> Result<(String, String), Verb> {
    if args.len() != 2 || !["claude", "codex"].contains(&args[0].as_str()) {
        return Err(Verb::die(usage, 2));
    }
    if !name_ok(&args[1]) {
        return Err(Verb::die(
            &format!(
                "'{}' is not an account name (letters, digits, . _ -)",
                args[1]
            ),
            2,
        ));
    }
    Ok((args[0].clone(), args[1].clone()))
}

/// `login_command`: the argv and the one environment variable of the login for an account folder.
fn login_command(h: &str, d: &Path) -> (Vec<&'static str>, (&'static str, String)) {
    let dir = d.to_string_lossy().into_owned();
    if h == "claude" {
        (vec!["claude", "auth", "login"], ("CLAUDE_CONFIG_DIR", dir))
    } else {
        (vec!["codex", "login", "--device-auth"], ("CODEX_HOME", dir))
    }
}

/// The exit code the way Python's `sys.exit(subprocess.call(...))` ends: a signal is its negative.
fn exit_code(s: std::process::ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        s.code()
            .or_else(|| s.signal().map(|n| 256 - n))
            .unwrap_or(1)
    }
    #[cfg(not(unix))]
    {
        s.code().unwrap_or(1)
    }
}

/// `harness_login`: the harness's own login, in its folder (`None`: the default login). Its exit code.
fn harness_login(h: &str, d: Option<&Path>) -> Result<i32, Verb> {
    if d.is_some() && h == "codex" {
        say(
            "This logs Codex in separately from your usual Codex login (device code; the browser step is yours).\n",
        );
    }
    let mut cmd;
    if let Some(d) = d {
        let (argv, (k, v)) = login_command(h, d);
        cmd = std::process::Command::new(argv[0]);
        cmd.args(&argv[1..]).env(k, v);
    } else {
        cmd = std::process::Command::new(h);
        cmd.args(if h == "claude" {
            &["auth", "login"][..]
        } else {
            &["login"][..]
        });
    }
    cmd.status()
        .map(exit_code)
        .map_err(|e| Verb::die(&format!("{h}: {e}"), 1))
}

fn make_folder(d: &Path) -> Result<(), Verb> {
    let made = std::fs::create_dir_all(d);
    // the folder holds a login: the owner only
    #[cfg(unix)]
    let made = made.and_then(|()| {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700))
    });
    made.map_err(|e| Verb::die(&format!("{}: {e}", d.display()), 1))
}

/// `join_set`: the account joins the global set at the end. With no account of its harness's family in the set yet, that
/// harness's default login goes in first: adding a second login must not take the first out of use. Its place.
fn join_set(ctx: &Ctx<'_>, entry: &str) -> Result<usize, Verb> {
    let h = super::parse(entry).map_err(|e| Verb::die(&e, 2))?.0;
    let own = if h == "claude-api" {
        "claude".to_owned()
    } else {
        h
    };
    edit_with(ctx, |s| {
        let mut ids = super::set_verbs::current(s, "");
        if !ids.iter().any(|i| super::in_family(i, &own)) {
            ids.push(own.clone());
        }
        if let Some(p) = ids.iter().position(|i| i == entry) {
            return Ok(p + 1);
        }
        ids.push(entry.to_owned());
        let n = ids.len();
        edits::set_accounts(s, "", Some(&ids));
        Ok(n)
    })
}

#[allow(clippy::many_single_char_names, clippy::similar_names)] // h/n/d/k/v are the verb's own names
pub(super) fn add(ctx: &Ctx<'_>, mut args: Vec<String>) -> Result<Verb, Verb> {
    let as_json = flag(&mut args, "--json");
    let yes = flag(&mut args, "--yes");
    if args.len() == 1 && args[0].starts_with("claude-api:") {
        let entry = args.remove(0);
        valid_id(&entry)?;
        let place = join_set(ctx, &entry)?;
        return Ok(Verb {
            stdout: format!("{entry} is account {place} in the set\n"),
            stderr: not_ready_note(ctx, &entry, ""),
            code: 0,
        });
    }
    let (h, n) = harness_name(
        &args,
        "account add claude|codex <name> [--yes] [--json] | account add claude-api:<profile>",
    )?;
    let d = super::account_dir(ctx, &h, &n);
    if !ctx.out.on_path(&h) {
        return Err(Verb::die(
            &format!("{h} is not installed here; install it first, then run this again"),
            1,
        ));
    }
    let created = !d.exists();
    if as_json {
        if h == "claude" && created && !yes {
            let out = J::Obj(vec![
                ("ok", J::Bool(false)),
                (
                    "error",
                    J::s(
                        "adding a second Claude subscription account needs --yes after the warning is shown",
                    ),
                ),
                ("warning", J::s(WARNING)),
            ]);
            return Ok(Verb {
                stdout: out.dumps() + "\n",
                code: 2,
                ..Verb::default()
            });
        }
        make_folder(&d)?;
        let (argv, (k, v)) = login_command(&h, &d);
        let mut f = vec![
            ("ok", J::Bool(true)),
            ("id", J::Str(format!("{h}:{n}"))),
            ("harness", J::Str(h.clone())),
            ("name", J::Str(n.clone())),
            ("dir", J::Str(d.to_string_lossy().into_owned())),
            ("created", J::Bool(created)),
            (
                "login",
                J::Obj(vec![
                    ("argv", J::Arr(argv.into_iter().map(J::s).collect())),
                    ("env", J::Obj(vec![(k, J::Str(v))])),
                ]),
            ),
        ];
        if h == "claude" {
            f.push(("warning", J::s(WARNING)));
        }
        return Ok(Verb::out(J::Obj(f).dumps() + "\n"));
    }
    if h == "claude" && created {
        say(&format!("{WARNING}\n"));
        if !yes && !["y", "yes"].contains(&ask("Add this account? Type y to continue: ").as_str()) {
            return Err(Verb::die("not added", 1));
        }
    }
    make_folder(&d)?;
    say(&format!("Login for {h}:{n} in {}\n\n", d.display()));
    let code = harness_login(&h, Some(&d))?;
    if code != 0 {
        return Err(Verb::die(
            &format!(
                "the login ended with exit {code}; run kith account add {h} {n} again to retry"
            ),
            code,
        ));
    }
    if h == "claude" {
        mirror::prepare_claude_dir(ctx, &d);
    }
    let place = join_set(ctx, &format!("{h}:{n}"))?;
    say(&format!(
        "\n{h}:{n} is account {place} in the set (kith account order|move changes the order)\n"
    ));
    Ok(Verb::default())
}

pub(super) fn login(ctx: &Ctx<'_>, mut args: Vec<String>) -> Result<Verb, Verb> {
    if args.len() != 1 {
        return Err(Verb::die("account login <id>", 2));
    }
    let entry = args.remove(0);
    valid_id(&entry)?;
    let (h, n) = super::parse(&entry).map_err(|e| Verb::die(&e, 2))?;
    if h == "claude-api" {
        let fix = super::fix(&entry, "").ok().flatten().unwrap_or_default();
        return Err(Verb::die(
            &format!("{entry} is an API key, not a login: {fix}"),
            1,
        ));
    }
    if h != "claude" && h != "codex" {
        return Err(Verb::die(&format!("{h} is not supported yet"), 2));
    }
    let d = n.as_deref().map(|n| super::account_dir(ctx, &h, n));
    if let (Some(d), Some(n)) = (&d, &n)
        && !d.is_dir()
    {
        return Err(Verb::die(
            &format!("no account {entry}; add it first: kith account add {h} {n}"),
            1,
        ));
    }
    if !ctx.out.on_path(&h) {
        return Err(Verb::die(&format!("{h} is not installed here"), 1));
    }
    let code = harness_login(&h, d.as_deref())?;
    if code != 0 {
        return Err(Verb::die(
            &format!(
                "the login ended with exit {code}; run kith account login {entry} again to retry"
            ),
            code,
        ));
    }
    if h == "claude"
        && let Some(d) = &d
    {
        mirror::prepare_claude_dir(ctx, d);
    }
    Ok(Verb::out(format!("{entry} is signed in\n")))
}

/// Whether the entry is in the global set, an agent's own list or a pin.
fn in_use(s: &Map, entry: &str) -> bool {
    let named = |v: Option<&serde_json::Value>| {
        v.and_then(serde_json::Value::as_array)
            .is_some_and(|a| a.iter().any(|i| i.as_str() == Some(entry)))
    };
    named(s.get("accounts"))
        || s.get("agents")
            .and_then(serde_json::Value::as_object)
            .is_some_and(|agents| {
                agents.values().any(|a| {
                    named(a.get("accounts"))
                        || a.get("pin")
                            .and_then(|p| p.get("id"))
                            .and_then(serde_json::Value::as_str)
                            == Some(entry)
                })
            })
}

pub(super) fn remove(ctx: &Ctx<'_>, mut args: Vec<String>) -> Result<Verb, Verb> {
    let as_json = flag(&mut args, "--json");
    let yes = flag(&mut args, "--yes");
    let usage = "account remove <id> [--yes] [--json]";
    let entry = match args.len() {
        2 => {
            let (h, n) = harness_name(&args, usage)?;
            format!("{h}:{n}")
        }
        1 => {
            valid_id(&args[0])?;
            args.remove(0)
        }
        _ => return Err(Verb::die(usage, 2)),
    };
    let (h, n) = super::parse(&entry).map_err(|e| Verb::die(&e, 2))?;
    let d = n
        .as_deref()
        .filter(|_| h == "claude" || h == "codex")
        .map(|n| super::account_dir(ctx, &h, n));
    if let Some(d) = &d
        && std::fs::symlink_metadata(d).is_ok_and(|m| m.file_type().is_symlink())
    {
        return Err(Verb::die(
            &format!("{} is a link; kith will not delete through it", d.display()),
            1,
        ));
    }
    let folder = d.as_ref().is_some_and(|d| d.is_dir());
    let used = in_use(&settings::read(ctx), &entry);
    if !used && !folder {
        return Ok(Verb::out(if as_json {
            J::Obj(vec![
                ("ok", J::Bool(true)),
                ("changed", J::Bool(false)),
                ("id", J::Str(entry.clone())),
            ])
            .dumps()
                + "\n"
        } else {
            format!(
                "{entry} is not in the set{}\n",
                if n.is_some() {
                    ""
                } else {
                    " (the default login is not deleted by kith)"
                }
            )
        }));
    }
    if let (true, false, Some(d)) = (folder, yes, &d) {
        if std::io::stdin().is_terminal() {
            let q = format!(
                "This deletes {} and the {h} login kept in it. Type y to continue: ",
                d.display()
            );
            if !["y", "yes"].contains(&ask(&q).as_str()) {
                return Err(Verb::die("not removed", 1));
            }
        } else {
            return Err(Verb::die(
                &format!(
                    "this deletes {} and the {h} login kept in it (nothing else). Run again with --yes.",
                    d.display()
                ),
                2,
            ));
        }
    }
    if used {
        edit_with(ctx, |s| {
            edits::drop_entry(s, &entry);
            Ok(())
        })?;
    }
    let mut out;
    match (folder, &d) {
        (true, Some(d)) => {
            std::fs::remove_dir_all(d)
                .map_err(|e| Verb::die(&format!("{}: {e}", d.display()), 1))?;
            if as_json {
                out = J::Obj(vec![
                    ("ok", J::Bool(true)),
                    ("changed", J::Bool(true)),
                    ("id", J::Str(entry.clone())),
                    ("dir", J::Str(d.to_string_lossy().into_owned())),
                    ("in_chain", J::Bool(used)),
                ])
                .dumps()
                    + "\n";
            } else {
                out = format!(
                    "removed {entry} from the set and deleted {}.\n",
                    d.display()
                );
                if h == "claude" {
                    out += "Claude Code may keep a Keychain item for it; `CLAUDE_CONFIG_DIR=<dir> claude auth logout` before removing drops that too.\n";
                }
            }
        }
        _ if as_json => {
            out = J::Obj(vec![
                ("ok", J::Bool(true)),
                ("changed", J::Bool(true)),
                ("id", J::Str(entry.clone())),
                ("in_chain", J::Bool(used)),
            ])
            .dumps()
                + "\n";
        }
        _ => {
            out = format!(
                "removed {entry} from the set{}\n",
                if n.is_some() || h == "claude-api" {
                    ""
                } else {
                    " (its login on this Mac is not touched)"
                }
            );
        }
    }
    Ok(Verb::out(out))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn an_account_folder_is_for_the_owner_only() {
        let root = std::env::temp_dir().join(format!("kith-login-mode-{}", std::process::id()));
        let d = root.join("accounts").join("claude").join("work");
        make_folder(&d).unwrap_or_else(|e| panic!("{}", e.stderr));
        let mode = std::fs::metadata(&d)
            .unwrap_or_else(|e| panic!("{e}"))
            .permissions()
            .mode();
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(mode & 0o777, 0o700);
    }
}
