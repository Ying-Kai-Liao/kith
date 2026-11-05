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

### Conversation verbs (M3-0d)

`golden/` also freezes what M3 replaces: `threads.json`, `delegation.json`, `accounts.json`, `accounts-core.json` (copies of the
corpora of the same names; the threads copy masks `ps` start times and, for `threads-record-*`, the claude pid found above the
hook, which depends on the machine's process tree), `wake.json` (kith-wake: enqueue, merge, dedupe, withdraw, withdraw-person,
take, done, back, ls, status, the `[kith wake RUN]` recovery rule, waiters `ci:` / `pid:` / `at:` fired by `drain` with a fake
`gh` and a held session so no turn starts, and `deliver` with a fake `kith`) and `sessions.json` (kith-sessions: resolve, claim,
terminal, proc, sweep, release, latest, pick).

`asks.json` (M3-10d) freezes `kith-ask` as the Python kernel prints and does it: `--answer` (answer, duplicate, progress, unknown id),
`--wait`, `--to` (queued, duplicate, unknown job, no text), `--end` by session and by person (the whole tree of that person, a second
`--end` ends nothing, delivered and woken asks stay), `delegations --json`, `--cap` and `--cap-told`. All cases are `unswitched`
(`kith-ask` has no Rust side yet); the core rows they will be replayed against are `ask.open`, `ask.settle`, `ask.end`, `ask.delegations`,
`ask.record`, `ask.limits` (already covered by `delegation.json`'s replay in `core/runtime/src/asks/tests.rs`) and `ask.to`. Not recorded: an
`ask <agent> "task"` that runs an agent (needs a real engine; the open and its caps are `delegation.json`'s `check` cases), and `--bg`
(detaches a process). Intended differences of the core, to list in `intended.json` when the CLI is switched:
- `--to` to an ended or stopped ask: Python queues the message (`queued`, exit 0, "still running") and the stopped fence drops it
  later; the core refuses at send with `was stopped; ask anew` (`asks-to-after-end-python-sends`, step 1, `ask.to`).
- `--end --person P`: the core also ends the asks of P's bots when the revoke order is used (`ask.revokeEverywhere`), which Python's
  `--end --person` does only for the persons it is given.
- `--end --session`: the core walks the subtree below the session's asks (`asks/mod.rs` `end_tree`); Python leaves a child ask (`r2` in
  `asks-end-usage`, step 1) open. That case goes red on the switch.
- `end_tree` sets `ended_ms` on answered and delivered asks too, so `ask.to` refuses a follow-up to answered `h1`/`h2` with "was stopped"
  (for an ask that was answered and not stopped: "was ended"); Python keeps them `answered` with no `ended_at`.

Extra case fields these use: `mask_keys` (names whose value means nothing: a number becomes `<EPOCH>`, anything else `<MASKED>`),
`normalize_paths` (the case's own `normalize` rules also apply to file names, for files named by an id the program makes up; two
files with one masked name are an error), and `unswitched` (text saying why the Rust side has no such verb yet). A golden replay
with `--rust` skips an `unswitched` case and prints `skip  NAME  (not switched: ...)`; `--require-switched` replays it, so it is
red until the verb exists. The Python replay always runs every case.

### project-id (M5-2)

`golden/project-id.json` (also in `corpus/`) freezes `kernel/bin/project-id` before `kith kernel project-id` replaces it: 19 cases of the
bare form (git and non-git folders, paths with spaces and non-ASCII names, symlinks to a folder and to a repo, a remote written five
ways, a worktree and a detached HEAD, nested aliases with the longest winning, a remote beating a path), `--list`, `--info`,
`--children [--all]`, `--where`, `--root`, `--register` (parent, home, bare `--home`, existing entries keeping their fields) and
its usage errors, with loops and a missing parent in the seeded `projects.json`. They run the Python as `program.old` and
`kith kernel project-id` as `program.new`, so the Rust side is compared without the verb being switched (no `core-switched.txt` line).
A malformed `projects.json` (`pid-malformed-projects`) is `unswitched` too: Python tracebacks, Rust prints one stderr line, both exit 1; JSON that Python accepts and serde rejects (`NaN`, `Infinity`) falls under the same difference (an intended difference; the golden replay does not read intended.json).
Not ported: the guest branch (`KITH_RUN_TOKEN` and `KITH_RUN_SOCKET` set: the host answers over the run socket). The core refuses there
with exit 2, so `pid-guest-branch-stays-python` is `unswitched` (Python replay only). Whoever switches the verb keeps guest calls on
Python (`runauth.rs` `project_args` and the run-tokens property tests ask the Python).

### Package resolve (M5-3)

`golden/pkg-resolve.json` freezes `kith-pkg resolve` (the launch path: `bin/kith` asks it for the pinned package dir) as the Python
kernel prints and does it: usage errors, an unknown agent, `--all`, one and several packages (role layout, plain layout, hidden and
empty folders), the snapshot (`prompt.md`, `settings.json`, `.home`, `.meta.json`, what the fingerprint skips), version pins,
`.editing` (fresh, stale, with no good version yet), the failure ladder (checks fail with a good version to fall back to, or without
one: an unchecked snapshot and its `.unchecked` note, then the second run), every check `problems()` makes, the fallback to the live
folder when the snapshot itself throws, linked third-party skills fetched from a local stand-in for GitHub (`KITH_GITHUB_GIT`), a
fetch that cannot reach its source, the commit of every good version into the agent's own repo, and the store's garbage collection.
`resolve` is not read-only: it fetches, snapshots, moves the `.good` pointer, collects old versions and commits.

How it is made hermetic:

- **The kernel.** Each case copies `bin/kith-pkg` into `home/k/bin` next to a linked `lib/` and `services/`, with its own one-line
  `PROTOCOL.md` and a stand-in `agents/scratch`, so a snapshot's `prompt.md` does not hold this checkout's protocol text. The Rust
  side runs `kith kernel pkg --kernel home/k resolve ...`.
- **`w.sh`.** One call: `$1` is a change to the agents made first (`-` for none, `a,b` for two; edit, break, pin, `.editing`, an old
  store), the rest is the line. After it, the store's listing (versions counted, their hash masked) goes to the fake log, because the
  folder names carry a fingerprint that differs per side (it hashes the agent's path and its files' mtimes).
- **Fingerprints.** Masked in text. Cases that make several versions of one agent leave the hashed folders out of the state
  comparison (`ignore`) and compare the listing instead; the others compare every file of the snapshot (`normalize_paths`).
  The algorithm itself is checked by the Rust unit tests against values the Python printed.
- **Python's own error texts.** An `invalid JSON (...)` reason is Python's `json` wording (`Expecting value: line 1 column 1 (char 0)`);
  the Rust side takes it from `secrets::pyjson::error_of`. Git's own words in a failed fetch are masked.
- **The one Python left.** The judging of `settings.json` allow rules against the must-ask classes (`kith_guard.must_ask_rule`)
  stays in Python until the host moves; the Rust side calls it only when `settings.json` has rules to judge.

### Schedules (M4-0)

`golden/sched.json` freezes `kith-sched` before M4 replaces it (39 cases, `unswitched`: "kith-sched has no Rust side yet"):
`ls` / `log` / `--json` (empty, mixed jobs, a table of 26 cron strings read through `ls --json` at three clocks), unreadable
`jobs.json` files, `add` (also as a replace, with `-m -e -H`, as a person, from an app run's outbox), `pause` / `resume` / `rm`,
`run` (while running, as a person, as the owner, a refused person), and `tick` at fixed clocks: a job due, the first tick with no
state, not due yet, catch-up after a sleep under a day (one run) and over a day (only what fell in the last 24 h runs), the edge
of that day, paused, bad cron, a run still going inside 6 h (skipped) and past it, a refused / revoked / unknown person, `0` and `7`
as Sunday, day-of-month together with day-of-week, the `@` macros, corrupt state, two agents, an epoch clock, the hourly prune.

How it is made hermetic:

- **The clock.** `kith-sched` takes `KITH_SCHED_NOW` (ISO local time or epoch seconds); unset it reads the real clock, and the runs it starts
  do not inherit it. Every tick case ends with `log --json`, so its result can be read without `state.json` once that moves to core.db. Each
  case line is `[CLOCK, ...verb]`, because one case needs several clocks: `w.sh` (written by the case's setup) exports it and
  runs the program. It starts `kith-sched` from a copy of the kernel's `bin/` in `home/k/` (so the tick's helpers resolve there)
  with `lib` linked, and on the Rust side runs `"$KITH_BIN" kernel schedule` (`KITH_CORE=rust`) with the same line. Whoever
  builds the Rust verb makes it honour `KITH_SCHED_NOW` and look for `kith`, `kith-ask` and `kith-wake` in `home/k/bin` (an
  env var of its own), as the Python tick calls `KITH/bin/kith-ask --sweep` and `KITH/bin/kith-wake drain`.
- **Side effects.** Those three are stand-ins in `home/k/bin`: `kith-ask` and `kith-wake` log their call to the fake log (so
  every tick shows `kith-ask --sweep` and `kith-wake drain`, never run), `kith` logs who the run was for (person, via, job, model,
  effort, harness) and a job called `slow-*` stays running until `home/.release` exists, which is how "still running" gets a live pid without a
  timer. A case ends such a run with the step `["-", "release"]` (handled by `w.sh`, not by `kith-sched`). `w.sh` waits for
  each started stand-in to have written its record, and numbers the runs' outbox folders (named by the nanosecond) `-n1`, `-n2`.
- **Masks.** `pid` (a key), the traceback line numbers and `(pid N)`; ISO times are kept readable (`2026-11-03@08:30`, a case
  `normalize` rule ahead of the generic `<TS>`), because the clock is the point. `auth.json` is left out where a revoke
  stamps the real time into it.

Recorded as today's behaviour, bugs included (a golden is not a spec; fix them in M4 and re-record with a reason):
`kith-sched log` in text mode crashes with `UnboundLocalError` (the name `log` is assigned later in `main`); `ls` of a job with
no occurrence in 366 days (`0 0 29 2 *`) prints `bad cron: 'str' object has no attribute 'strftime'`; the 6 h rule compares
`at` (minutes) with an ISO string that has seconds, so a run exactly 6 h old counts as old. Daylight saving (cases
`sched-tick-dst-*`, `TZ=Australia/Sydney`, epoch clocks): kith-sched works in local time without a zone, so a job inside the
skipped hour fires at 03:00 as `caught up after sleep` (02:30 never happens), and in the repeated hour the tick at the second
02:00 is `clock behind` and returns, then 02:01 on runs again: a `30 2 * * *` job fires twice, once per pass. (The stamps stay
right: an epoch clock keeps the second pass's fold.)

`fixtures/routines-next.json` freezes the host's routines timing (`serve/host/src/routines/schedule.rs`): 132 rows of
`Trigger::next(after)` (Cron in many IANA zones and across DST changes of New York, London, Sydney and Lord Howe, Interval,
Once, Webhook / Event) with its `validate` verdict, and `Trigger::advance(due, now)`. The Rust test
`routines::schedule_golden::routines_next_fire_times_are_frozen` (run by CI's `-f packages="kith-serve"`) fails when any answer
changes. Re-record on purpose: `KITH_RECORD_ROUTINES_NEXT=1 cargo test -p kith-serve --bin kith-serve schedule_golden`, and say why in the PR.
Note that the host rejects the `@` macros (it needs five fields) while kith-sched accepts them; M4 has to decide, the fixture records both.
The fixture lives in `fixtures/`, not `golden/`, because the golden loader reads every `golden/*.json` as a list of cases.

Bug included on the host side, fixed by #765 (main, `0f1d5968`): `30 1 * * *` in America/New_York, `next` after 2026-11-01T06:00Z
(01:00 EST, the second pass of the repeated hour) used to answer 05:30Z, which is *before* `after` (croner picks the earlier, EDT,
reading of 01:30), so a missed fire left a due time in the past. The host now answers 06:30Z (never at or before `after`); the
fixture was re-recorded for that, and the `advance` row that showed the bug changed with it. M4's Trigger must keep the same rule.

### Cost (M5-1)

`golden/cost.json` (also in `corpus/`) freezes the verbs of `kith-cost` that the launch and the apps call: `limit` (text, `--json`,
`--sh`), `spent`, `usage --json`, `agent NAME --json`, `members`, `bots --json`, `sync`, and the usage text. The cases are
`needs: rust` with `program` set (`kernel/bin/kith-cost` against `{BIN} kernel cost --kernel {KERNEL}`), not `unswitched`: the Rust side exists, only
the switch (a `cost` line in `lib/core-switched.txt`) is not made. Ledgers are built from transcripts (`home/.claude/projects/**`, with
subagent `.meta.json`), a hand-written `cost.db`, the bridge's `state.json`, an extra Claude account, `settings.json` limits and an `auth.json`.

**The clock.** `kith-cost` takes `KITH_COST_NOW` (ISO local time or epoch seconds; unset it reads the clock) for the calendar month, the
periods, the forecast and the limit's month. Cases pin it with `TZ`, including Sydney across the daylight-saving start (a month's midnight keeps
the offset of "now", as the Python does), New York, Lord Howe, December and February. `tag` and `record` keep the real clock.

Not covered (no Rust side yet, `kith kernel cost` exits 2): `today|week|month|all`, `agents`, `agent NAME` as text, `people`, `session`, `csv`,
and the hooks `tag` / `record`. A pace so small that a limit lies years away makes the Python's `forecast` raise `OverflowError`; no case records
that traceback, and the core answers "stays under" (unit test `a_tiny_pace_never_reaches_the_limit`).

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

Git noise: `.git/index`, `.git/objects/*`, `.git/hooks/*` and `.git/config` are never compared (they vary with the runner's git version and OS, not with kith); HEAD, refs and logs are. See `GIT_NOISE` in `kith-difftest`.

## Intended differences

`intended.json` is a list of `{"case": glob, "facet": glob, "why": text}`. Facets are named `stepN.exit`,
`stepN.stdout` / `stepN.json`, `stepN.stderr`, `state:<path under the side root>` and `fakes`. A difference that
matches no entry fails. An entry that matches no difference in the cases that ran is reported as `STALE` and fails the
run, so the list shrinks when a difference goes away. Every entry needs a reason (a ticket, a B-step of the plan).

## Self-test

`selftest.py` checks the runner itself: the same line is the same, a deliberately different NEW side fails with a diff,
the same difference listed as intended passes, an unused entry is stale. Run by `kernel/tests/kith-difftest-check`.
