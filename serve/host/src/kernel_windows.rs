//! What `kith <verb>` answers on Windows for the verbs the host does not own (`Route::Kernel`). On macOS and Linux those go to
//! the kernel's bash entry; Windows has no bash fallback (windows-owner WI2: nothing kith starts is a script), so each verb is
//! either ported (it runs in this program) or refused by name with the retire-python package that ports it (WI1: no silent
//! no-op). The table is data only: a port removes one row and, if the verb now runs here, adds it to the ported rows.
//!
//! The table compiles everywhere so Unix tests can check it against `kernel/bin/kith`; only `exec_kernel` on Windows uses it.
#![cfg_attr(unix, allow(dead_code))]

use std::ffi::OsString;

/// What one verb does on Windows.
#[derive(Debug, PartialEq, Eq)]
pub enum Row {
    /// Runs in this program: the host arguments that answer it, before the caller's own arguments.
    Ported(&'static [&'static str]),
    /// Not yet: the retire-python package (`Pn-m`) that ports it.
    Refused(&'static str),
}

/// The verbs `kernel/bin/kith` knows, grouped by what they share. `FALLBACK` answers anything else: a word that is no
/// verb is an agent's name there, and running an agent is the run path.
pub const ROWS: &[(&[&str], Row)] = &[
    (&["engines"], Row::Ported(&["kernel", "engines"])),
    // The run path: `kith <agent> ...`, held terminals, the agent list.
    (&["attach", "ls"], Row::Refused("P1-12")),
    (&["account"], Row::Refused("P1-14")),
    (&["migrate"], Row::Refused("P4-22")),
    (&["versions", "pin", "unpin", "rollback", "install", "history", "restore", "deps"], Row::Refused("P4-1")),
    (&["push", "commit", "share", "search", "market"], Row::Refused("P4-2")),
    (&["update"], Row::Refused("P4-15")),
    (&["skills", "skill"], Row::Refused("P4-3")),
    (&["scratch"], Row::Refused("P4-4")),
    (&["project"], Row::Refused("P4-5")),
    (&["group", "bot"], Row::Refused("P4-6")),
    (&["connector"], Row::Refused("P4-7")),
    (&["serve", "link", "unlink"], Row::Refused("P4-8")),
    (&["new"], Row::Refused("P4-11")),
    (&["doctor"], Row::Refused("P4-12")),
    (&["init"], Row::Refused("P4-13")),
    (&["backup"], Row::Refused("P4-14")),
    (&["keys", "who"], Row::Refused("P4-16")),
    (&["bug"], Row::Refused("P4-17")),
    (&["remote"], Row::Refused("P4-20")),
    (&["acp"], Row::Refused("P4-21")),
    (&["auth", "connect", "disconnect", "connectors"], Row::Refused("P3-2")),
    (&["cost", "people", "invite"], Row::Refused("P3-1")),
    (&["door"], Row::Refused("P3-12")),
    (&["allow"], Row::Refused("P3-5")),
    (&["cards"], Row::Refused("P3-6")),
    (&["send-to"], Row::Refused("P3-7")),
    (&["recall", "summarise", "forget", "memory"], Row::Refused("P3-8")),
    (&["ask"], Row::Refused("P2-3")),
    (&["schedule"], Row::Refused("P2-5")),
    (&["wake"], Row::Refused("P2-2")),
];

/// For a word no row names (an agent's name) and for no verb at all: the run path.
pub const FALLBACK: &str = "P1-12";

/// What to do with a command line the router sent to the kernel.
#[derive(Debug, PartialEq, Eq)]
pub enum Answer {
    /// Run this program with these arguments.
    Run(Vec<OsString>),
    /// Say this on stderr and exit 1.
    Refuse(String),
}

fn refusal(verb: &str, package: &str) -> Answer {
    Answer::Refuse(format!("not on Windows yet: {verb} (retire-python {package})"))
}

/// The answer for `args` (what follows `kith`).
pub fn answer(args: &[OsString]) -> Answer {
    let verb = args.first().map_or_else(|| "(no verb)".to_owned(), |a| a.to_string_lossy().into_owned());
    let row = ROWS.iter().find(|(verbs, _)| verbs.contains(&verb.as_str())).map(|(_, r)| r);
    match row {
        Some(Row::Ported(lead)) => {
            Answer::Run(lead.iter().map(OsString::from).chain(args[1..].iter().cloned()).collect())
        }
        Some(Row::Refused(package)) => refusal(&verb, package),
        None => refusal(&verb, FALLBACK),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::{Route, route};
    use std::collections::BTreeSet;

    fn v(a: &[&str]) -> Vec<OsString> {
        a.iter().map(OsString::from).collect()
    }

    fn verbs() -> BTreeSet<&'static str> {
        ROWS.iter().flat_map(|(vs, _)| vs.iter().copied()).collect()
    }

    /// Every verb the bash router names has a row: the word list it suggests from, and the verbs it handles by name.
    #[test]
    fn every_kernel_verb_has_a_row() {
        let sh = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../kernel/bin/kith")).unwrap();
        let line = sh.lines().find(|l| l.starts_with("SUBCOMMANDS=\"")).expect("SUBCOMMANDS in kernel/bin/kith");
        let words = line.trim_start_matches("SUBCOMMANDS=\"").trim_end_matches('"');
        let have = verbs();
        for w in words.split_whitespace() {
            assert!(have.contains(w), "no Windows row for the verb `{w}`");
        }
        for w in ["attach", "connect", "disconnect", "connectors", "skill", "skills", "scratch", "engines"] {
            assert!(have.contains(w), "no Windows row for the verb `{w}`");
        }
    }

    #[test]
    fn a_verb_is_in_one_row_and_a_refusal_names_a_package() {
        let mut seen = BTreeSet::new();
        for (vs, row) in ROWS {
            for w in *vs {
                assert!(seen.insert(*w), "`{w}` has two rows");
            }
            match row {
                Row::Refused(p) => {
                    let (n, m) = p[1..].split_once('-').expect(p);
                    assert!(p.starts_with('P') && n.parse::<u32>().is_ok() && m.parse::<u32>().is_ok(), "{p}");
                }
                Row::Ported(lead) => assert!(!lead.is_empty()),
            }
        }
    }

    /// Every command line `route` sends to the kernel gets an answer from the table: the serve verbs the kernel owns, each
    /// verb, an agent's name and no argument at all.
    #[test]
    fn every_routed_kernel_line_has_an_answer() {
        let mut lines: Vec<Vec<OsString>> = vec![v(&[]), v(&["coder", "-p", "x"]), v(&["serve"])];
        for sv in ["start", "stop", "restart", "status", "relay", "help", "-h", "--help"] {
            lines.push(v(&["serve", sv]));
        }
        for w in verbs() {
            lines.push(v(&[w, "x"]));
        }
        let mut kernel_lines = 0;
        for line in lines {
            let mut argv = v(&["kith"]);
            argv.extend(line.iter().cloned());
            let Route::Kernel(routed) = route(&argv) else { continue };
            kernel_lines += 1;
            match answer(&routed) {
                Answer::Refuse(m) => assert!(m.starts_with("not on Windows yet: ") && m.ends_with(')'), "{m}"),
                Answer::Run(a) => assert_eq!(a, v(&["kernel", "engines", "x"])),
            }
        }
        assert!(kernel_lines > 20, "{kernel_lines}");
    }

    #[test]
    fn the_refusal_text_is_exact() {
        assert_eq!(answer(&v(&["doctor"])), Answer::Refuse("not on Windows yet: doctor (retire-python P4-12)".into()));
        assert_eq!(answer(&v(&["serve", "start"])), Answer::Refuse("not on Windows yet: serve (retire-python P4-8)".into()));
        assert_eq!(answer(&v(&["coder", "-p", "x"])), Answer::Refuse("not on Windows yet: coder (retire-python P1-12)".into()));
        assert_eq!(answer(&v(&[])), Answer::Refuse("not on Windows yet: (no verb) (retire-python P1-12)".into()));
    }

    #[test]
    fn a_ported_verb_runs_with_its_own_arguments() {
        assert_eq!(answer(&v(&["engines", "--json"])), Answer::Run(v(&["kernel", "engines", "--json"])));
    }
}
