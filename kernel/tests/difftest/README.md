# kith-difftest: the same CLI line, the same result

Runs each corpus case on an OLD side and a NEW side, each in its own throwaway `HOME`, `KITH_HOME` and `TMPDIR`
(built from the case's setup, with fake `gh`, `security`, `launchctl`, `open`, `osascript`, `claude`, `codex` first on
`PATH`), and compares:

- what it printed: stdout (or `--json` as data, keys sorted), stderr, the exit code, per command line;
- what it did: every file under the side's home, kith home and tmp (SQLite stores by table rows, sorted; `*.json` as data);
- what it asked of the outside world: the log of fake calls (`fakes`).

Both sides are normalised first: the throwaway roots (`<HOME>`, `<KITH_HOME>`, `<TMP>`, `<ROOT>`), ISO timestamps,
epochs, uuids, long hex ids, plus the case's own `normalize` rules. The environment is built from scratch: nothing
`KITH_*` or `HQ_*` of the real machine reaches a run.

```
kernel/tests/difftest/kith-difftest                 the corpus; OLD = kernel/bin/kith (KITH_CORE=python), NEW = same (KITH_CORE=rust)
kernel/tests/difftest/kith-difftest --rust [--bin KITH]   also the cases marked "needs": "rust"
kernel/tests/difftest/kith-difftest --old 'CMD' --new 'CMD'   any two command prefixes (case arguments are appended)
kernel/tests/difftest/kith-difftest --case 'auth-*' --keep     some cases; keep the throwaway folders to look at
kernel/tests/difftest/kith-difftest --summary out.json         green/red per command (see below)
kernel/tests/kith-difftest-check                    self-test + the corpus, as CI runs it
```

Which side is which: `KITH_CORE=python` is today's kernel. `KITH_CORE=rust` routes a command to the core where the core
implements it (`kernel/lib/kith-core.sh`; `kernel/bin/kith` calls it for `paths` so far). The NEW side finds the Rust
program through `KITH_BIN`, which the runner sets from `--bin`, `$KITH_DIFFTEST_BIN`, or a cargo build in `$CARGO_TARGET_DIR`
/ `serve/host/target`. Cases for verbs the core does not have yet are the same program on both sides, which proves the
runner is deterministic. Or point `--new` at any other program.

## Golden corpus (access verbs)

`golden/` freezes the access verbs' cases (copies of json-auth, people, secrets-read and the seed's `auth-ls-empty-and-granted`,
plus `auth-invite-bot.json`: `kith-auth who|whois|retired|people` and the `invite` / `bot` `--json` usage errors) with what the
Python kernel printed and did, so the check survives the day authority moves into core.db and Python is no longer there to compare to.

    kith-difftest --golden golden --record      run the OLD (Python) side alone, store its facets in golden/facets/NAME.json
    kith-difftest --golden golden               replay on the Python kernel against the store (green = the record still holds)
    kith-difftest --golden golden --rust --bin PATH   replay the Rust program against the same store

Same masks as the two-sided run, plus the kernel checkout's path (`<KERNEL>`). `intended.json` is not read: a frozen record has
no side to differ on purpose, so re-record (and say why in the PR) when a change is meant. Cases are copies: editing corpus/ does
not move the golden.

## Per-command summary

`--summary FILE` (`-` for stdout) writes `{"commands": {"<command>": {"green": bool, "cases": {"<case>": "same" |
"intended" | "differs" | "error"}}}, "stale": [...]}`. A command is green when all its cases are same or intended. The
command of a case is its `command` field, else the first word of its first line. A command switches to the core when
it has been green across recent merges; the history is kept by whoever stores these summaries (CI artifact).

## Adding a case

Add an object to a file in `corpus/` (a list per file). Fields:

| field | meaning |
|---|---|
| `name` | unique |
| `command` | which command it is evidence for (default: first word of the first line) |
| `setup` | steps: `{"write": path, "text": ..., "mode": "755"}`, `{"mkdir": path}`, `{"link": path, "to": target}`, `{"sqlite": path, "sql": ...}`, `{"fake": "gh", "script": "#!/bin/sh..."}` (a program of your own ahead of the shared fakes), `{"run": [argv], "cwd": ...}` (e.g. `git init`). Paths are relative to the side's root (`home/`, `kith/`, `tmp/`); `{HOME}`, `{KITH_HOME}`, `{TMP}`, `{ROOT}` in text, targets, scripts and args are that side's own paths |
| `run` | list of command lines, each a list of arguments after the program (`["auth", "ls"]`) |
| `env` | extra environment (values may use the `{...}` paths; `null` leaves a variable out, e.g. the default `KITH_HOME`) |
| `stdin` | text for stdin (default: empty, never a TTY) |
| `cwd` | folder to run in, relative to the root (default `home`) |
| `normalize` | extra `[regex, replacement]` pairs, applied to every text of both sides |
| `ignore` | regexes of state paths to leave out (e.g. `"/logs/"`) |
| `sort_lists` / `sort_lines` | order means nothing: sort JSON lists / stdout lines (and the log of fake calls) |
| `stderr` | `false` to leave stderr out |
| `json` | `false` to compare stdout as text even when it parses |
| `program` | `{"old": [argv...], "new": [argv...]}`: the program a side runs instead of `kith` (the case's lines are appended), for a verb the router does not reach yet, e.g. `python3 {KERNEL}/lib/kith_accounts.py` against `{BIN} kernel accounts`; `{KERNEL}` is the kernel folder, `{BIN}` the Rust program |
| `needs` | `"rust"`: only run with `--rust` or `--new` |

Never put a bare `kith backup`, `update` or `migrate` in a corpus line unless acting is the point of the case; `kith
serve` must not start a host (use `help`/`status`-like lines). Everything is a throwaway home, so acting is safe, but
launchd, the Keychain and the network are fakes only.

## Intended differences

`intended.json` is a list of `{"case": glob, "facet": glob, "why": text}`. Facets are named `stepN.exit`,
`stepN.stdout` / `stepN.json`, `stepN.stderr`, `state:<path under the side root>` and `fakes`. A difference that
matches no entry fails. An entry that matches no difference in the cases that ran is reported as `STALE` and fails the
run, so the list shrinks when a difference goes away. Every entry needs a reason (a ticket, a B-step of the plan).

## Self-test

`selftest.py` checks the runner itself: the same line is the same, a deliberately different NEW side fails with a diff,
the same difference listed as intended passes, an unused entry is stale. Run by `kernel/tests/kith-difftest-check`.
