//! `kith kernel account ...`: every verb of `kernel/bin/kith-account`, the state ones (`list|pick|out|clear|usage`, in
//! `verbs.rs`) and the ones that write the set (`edits`, `set_verbs`, `backups`, `logins`). Same words, same exit codes.

use super::verbs::{Verb, migrate, run_verb};
use super::{Ctx, J, backups, logins, set_verbs};

const USAGE: &str = include_str!("usage.txt");

/// With `--json`, a failure is one `{"ok": false, "error": ...}` object on stdout (`JSON_ERRORS`), the exit code the same.
fn json_errors(as_json: bool, v: Result<Verb, Verb>) -> Verb {
    match v {
        Ok(v) => v,
        Err(v) if as_json && v.stderr.starts_with("kith: ") => {
            let msg = v.stderr["kith: ".len()..].trim_end_matches('\n');
            let out = J::Obj(vec![("ok", J::Bool(false)), ("error", J::Str(msg.to_owned()))]);
            Verb { stdout: out.dumps() + "\n", stderr: String::new(), code: v.code }
        }
        Err(v) => v,
    }
}

/// Runs one `kith account` command line. `stdin` supplies the reading of `usage ID --json -`.
pub fn run_account(ctx: &Ctx<'_>, mut args: Vec<String>, stdin: &dyn Fn() -> String) -> Verb {
    if args.is_empty() || ["-h", "--help"].contains(&args[0].as_str()) {
        return Verb::out(USAGE.to_owned());
    }
    let verb = args.remove(0);
    let known = [
        "add", "login", "list", "ls", "remove", "rm", "order", "move", "set", "policy", "pin", "cross-harness",
        "unpin", "migrate", "out", "clear", "usage", "pick", "backup", "use",
    ];
    if !known.contains(&verb.as_str()) {
        return Verb::die(&format!("unknown command: {verb} (kith account --help)"), 2);
    }
    // a settings file still in the older shape is rewritten in the new one before every command
    migrate(ctx);
    let as_json = args.iter().any(|a| a == "--json");
    match verb.as_str() {
        "list" | "ls" | "pick" | "out" | "clear" | "usage" => run_verb(ctx, &verb, args, stdin),
        "add" => json_errors(as_json, logins::add(ctx, args)),
        "remove" | "rm" => json_errors(as_json, logins::remove(ctx, args)),
        "backup" => json_errors(as_json, backups::backup(ctx, args)),
        "login" => json_errors(false, logins::login(ctx, args)),
        "order" => json_errors(false, set_verbs::order(ctx, args)),
        "move" => json_errors(false, set_verbs::move_(ctx, args)),
        "set" => json_errors(false, set_verbs::set(ctx, args)),
        "policy" => json_errors(false, set_verbs::policy(ctx, args)),
        "cross-harness" => json_errors(false, set_verbs::cross_harness(ctx, args)),
        "migrate" => json_errors(false, set_verbs::migrate_verb(ctx)),
        "pin" => json_errors(false, set_verbs::pin(ctx, args)),
        "unpin" => json_errors(false, set_verbs::unpin(ctx, args)),
        _ => json_errors(false, set_verbs::use_(ctx, args)),
    }
}
