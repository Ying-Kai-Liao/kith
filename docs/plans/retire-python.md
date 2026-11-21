# Retiring the Python kernel

Status: tracked series P0-P6, rewritten 2026-11-21. Phase 0 and Phase 1 run now; Phase 2 is not started (the architect is
designing S4 first); Phases 3-6 wait. The first version of this plan (2026-11-17, R1-R10, #1025) is superseded: section 9
maps it to the new ids, and the finished work of R1-R5 is in the log at the end.

Why now: Kai, 2026-11-21: "I already think we can retire python one now. just dont know whats the gap, since some of the
gap might be rebuild with newer idea." The architect's gap analysis of 2026-11-21 (code at `origin/rust-rebuild`
9b30143d6, live facts from Kai's Mac) is the evidence for every verdict below. It lives outside the repo, in the architect's
files: `/Users/Kath/kith/agents/architect/files/kith/designs/2026-11-21-python-gap/gap-analysis.md`. Kai chose "Phase 0 + 1
now" and "a full plan like the C series". On which features to drop (kith-remote, kith-acp and its shim, guest doors,
kith-pkg marketplace verbs, the hq rename steps) Kai said "Not sure": **nothing is dropped now; each is ported unless Kai
drops it, and he is asked when the item comes up** (`kith ask product-lead`).

Read with: `docs/plans/one-bot-model.md` (M5 is this series; its dated log), `docs/plans/core-design.md`,
`docs/plans/runtime-design.md`, `core/runtime/src/cutover/mod.rs` (the framework) and `docs/codebase-map.md`.

## Rules for every item

1. **Every merged PR ticks its item in this doc (status plus PR number) and adds a dated line to the log at the end.**
2. **The MacBookPro must switch all five modules (accounts, authority, claims, transcripts, schedules;
   `kith kernel cutover status --require` green) before any release deletes Python there.** This Mac already has.
3. A Python module is deleted in the PR that removes its **last user** (import, `SourceFileLoader`, `python3 <path>`,
   `uv run --script`, the plugin, plists). Find users with `python3 -I scripts/py-file-map.py`; it matches by name across
   every tracked text file, so a path built with `join` or a Rust string still shows. Paste the new table in the PR.
4. The deleting PR also: freezes the script's difftest corpus as a golden (`kernel/tests/difftest`), updates the drift lists
   (`verification/*/drift.sh` `FILES`) and skill trigger text of the TLA+ models that name it, the `ci.yml` path filters
   and checks (`kernel/tests/kith-ci-names-check` fails if a check is in neither `ci.yml` nor `ci-exempt.txt`), and
   `.github/kith-protected-paths`; and adds **no core.db migration** (P0-12 checks it).
5. A step run twice is harmless: deletions are idempotent, Mac-side actions go through the cutover framework or an
   installer action that is safe to run twice.
6. Mark: an item marked **needs architect design first** gets no worker until the architect's design is in.

## Gates (Kai is the only user)

Decided 2026-11-21 (Kai, via main). Kai updates his two Macs himself, so most of the multi-user safety net of the first
plan goes.

**Kept**
- The installer check: `scripts/install.sh` runs the new program's `kith kernel cutover status --require` before it flips
  `cli/current` (exists since Step 0, #1050). It is the one gate that stops a Python-free release from stranding unswitched
  state on the MacBookPro or on a stranger who ran the public installer.
- Goldens frozen in the PR that deletes a script (rule 4).
- TLA+ model drift lists and CI filters updated in the deleting PR (rule 4).
- The `python-kernel-final` tag (P0-11): free, and the only undo for a source checkout.
- The `MIN_CLI` raise (P5-6): one constant (`serve/host/src/kernel.rs`).
- A CI check that a deletion PR adds no core.db migration (P0-12), so "install the previous folder" stays a rollback.

**Dropped**
- The two-release compatibility window. Stubs live only where a live caller names the path: launchd plists (until they are
  rewritten), agent tools (`kith-send` is named by resource-manager `watch.py` and devops `uptime.py`), and the hooks of
  sessions still running at the update (a hook command is fixed for the life of a run).
- The publish gate across an owner list. It becomes a two-line checklist: this Mac, the MacBookPro.
- The host start-up refusal with a built-in required-module list. `kith doctor` naming what is not switched is enough.
- `kith update --back` and its hold marker. Rollback = re-point `cli/current` at the old folder
  (`KITH_KEEP_VERSIONS` keeps three) and run `kith serve install` from the old binary, by hand; two lines, written in P0-13.
- The R10 two-release state-format floor: the importers go once both Macs have stepped every module (P6).
- The loader-to-subprocess prep: moot when `kith-info`, `kith-ask`, `kith-remote` and `kith-skills` are redesigned or
  ported whole.

Caveat: anyone else who installed from kithbot.app (v0.1.3, Python) is only protected by the installer check.

## How to read an item

`- [ ] **Pn-m. Title.** what. Verdict. Size. Depends. Owner/PR. Status.` Verdicts: **DELETE NOW** (dead or fully replaced,
a repoint away), **PORT AS-IS** (small, design fine), **REDESIGN** (the core has a better shape; needs architect design
first), **DROP-candidate** (written as "PORT unless Kai drops it": ask Kai via `kith ask product-lead` when the item comes
up). Size: S under a day of a coder, M one to three PRs, L a design plus several PRs.

## Phase 0: now (installs, one-line repoints, dead files)

- [x] **P0-1. This plan.** Written as R1-R10. DONE. PORT. S. Depends: none. #1025. Done 2026-11-17 (rewritten as this series 2026-11-21).
- [x] **P0-2. Step 0.** `kith kernel cutover status MODULE|--json|--require FILE`, `kernel/lib/requires-switched.txt`, `scripts/py-file-map.py`. Verdict: PORT AS-IS. S. Depends: none. #1050. Done 2026-11-19.
- [x] **P0-3. R1 leaves.** `kith_engines.py`, `hq_settings.py`, the `python3_abs` fallback and the Python read-fence fallback of the Rust launch deleted. DELETE NOW. S. Depends: none. #1059. Done.
- [x] **P0-4. `kith-account` becomes a stub.** `bin/kith-account` is `exec kith account "$@"`; the `account-state` and `kith backup` Python fallbacks are gone. PORT AS-IS. S. Depends: none. #1081. Done (the stub itself goes in P0-6).
- [ ] **P0-5. Delete `kernel/lib/hq_paths.py`.** No caller in the repo; devops `monitor.py` now calls `kith paths --json`. DELETE NOW. S. Depends: none. Owner: retire-python-now-p0. Status: open.
- [ ] **P0-6. Delete the `kith-account` stub.** A stub window is pointless for one user; drop `kith-account-stub-check`, `kith-accounts-tombstone-check` and the difftest rows that only prove the stub, from `ci.yml` and `ci-exempt.txt` in the same PR. `kith_accounts.py` stays (P1 and P5). DELETE NOW. S. Depends: P0-4. Owner: retire-python-now-p0. Status: open.
- [ ] **P0-7. `kith-migrate` forwarder.** Route `kith migrate step|status|rollback` and `kith rollback MODULE` straight to `kith kernel cutover`; delete the forwarding part of `kith_migrate.py` (`:869-887`). The hq-to-kith rename steps stay until P4-22. DELETE NOW. S. Depends: none. Owner: retire-python-now-p0. Status: open.
- [ ] **P0-8. `kith-lifecycle` and `kith_lifecycle.py`.** Repoint `core/kernel/src/threads/run.rs:267` (`python3 kith-lifecycle sweep`) to `kith kernel lifecycle`, repoint or drop the two Python callers (`kith-wake:442`, `kith-sessions:968`, which go anyway), delete both files, replace the Python entry in the `thread-writers` model's `FILES`. DELETE NOW. S. Depends: #1086 (done). Owner: retire-python-now-p0. Status: open.
- [ ] **P0-9. Install the head build on Kai's Mac.** `cli/current` is `0.1.4-rust.02a1921`, two days behind: its program lists no `threads-writer`, `threads-run`, `lifecycle`, `room-ledger`, `cost-ledger`, `project-guest` or `account`, and `core-switched.txt` has no `threads`, `lifecycle`, `room-ledger` or `account` line, so the merged ports are not live here. PORT AS-IS (an action). S. Depends: none. Owner: main. Status: open.
- [ ] **P0-10. Read the MacBookPro cutover status.** `kith kernel cutover status` there, its branch, and L1381 (its host-native bot count). Rule 2 needs it green before any deletion release reaches it; a Mac on a main checkout gets the head kernel by `git pull`. PORT AS-IS (an action). S. Depends: none. Owner: main. Status: open.
- [ ] **P0-11. Tag `python-kernel-final`** on the last commit that still has every Python file (before the first deletion PR of P0 merges; with Kai's OK). PORT AS-IS (an action). S. Depends: none. Owner: main. Status: open.
- [ ] **P0-12. CI check: a deletion PR adds no core.db migration.** Compare `core/runtime/src/migrations` with the base's when kernel files are deleted. PORT AS-IS. S. Depends: none. Owner: kith devops/ci. Status: open.
- [ ] **P0-13. Write the two-line rollback.** Re-point `cli/current` at the old folder, `kith serve install` from the old binary; in `docs/` where the install notes live. PORT AS-IS. S. Depends: none. Owner: unowned. Status: open.

## Phase 1: the run path (the keystone; biggest win per PR)

Today every `kith` call runs `python3 kith_paths.py --sh` first (`kernel/bin/kith:146`); the interactive `kith AGENT` runs the
whole bash+Python launch (9 Python starts that always happen, 15 with the default ones); only `--launch-json` execs the Rust
launch (`kernel/bin/kith:353`); even that runs `kith-auth mcp-config` (`core/kernel/src/launch/assemble.rs:1484`) and wraps
every shared MCP server in `kith-auth exec` (`kith-auth:853-857`). Chat-platform turns and card resumes run `kith AGENT --
prompt` (`channels/run.rs:397`, `cards/resume.rs:892`), so they pay the same. Phase 1 needs no TLA+ model: it ports an
assembled settings file, and the claim and lock path is already Rust.

- [x] **P1-1. `project-id` in Rust.** Guest client `core/kernel/src/project/guest.rs`; `bin/project-id` is an sh stub (kept: agents and PROTOCOL.md run it by name). PORT AS-IS. M. Depends: none. #1077. Done 2026-11-19.
- [x] **P1-2. `kith-room-ledger` hook and `pkg resolve` without Python.** The launch writes the Rust room-ledger hook; `bin/kith-room-ledger` is a stub (goes in P5-7); `pkg resolve` has no Python fallback. PORT AS-IS. M. Depends: none. #1080. Done 2026-11-19.
- [x] **P1-3. Every `kith-cost` verb in Rust** (`tag`, `record`, the text reports; module `cost-ledger`). PORT AS-IS. L. Depends: none. #1073. Done.
- [x] **P1-4. `kernel/bin/kith-cost` becomes a forwarding stub** (one file valid as sh and Python, so the hand-written `dev.kith.cost` plist keeps working). PORT AS-IS. S. Depends: P1-3. #1098. Done 2026-11-21.
- [x] **P1-5. `bin/kith` and `kith-core.sh` call `kith kernel threads`** instead of `kith-sessions` (all verbs except `run`; both claim modes; the record hook). PORT AS-IS. M. Depends: none. #1083. Done.
- [x] **P1-6. `kith-sessions run` in Rust** (`kith kernel threads run`, module `threads-run`: take-over note, idle stop, status pipe, quit intent, claim release, sleep sweep, `wake_owed`). PORT AS-IS. L. Depends: P1-5. #1090. Done.
- [ ] **P1-7. `kith_paths.py` out of bash and the host.** The bash finds the program at a fixed relative path (`$KITH/../../../bin/kith`, as the stubs already do) instead of `python3 kith_paths.py --sh|--bin|--machine` (`kernel/bin/kith:146,331,468,924,1544`, six sites in `kernel/lib/kith-core.sh`: `:35,59,76,98`); `kernel paths --sh` exists (`kernel/bin/kith:137`). The host uses its in-process paths and deletes its two `python3` calls (`serve/host/src/runauth.rs:558`, `cards/resource.rs:160`, each has a Rust fallback). The plugin's call (`register.tsx:59`) is P4-23. The file itself goes in P5-3. PORT AS-IS. S. Depends: none. Owner: retire-python-now-paths. Status: in flight.
- [ ] **P1-8. `kith_guard` must_ask into Rust.** Port `must_ask` into `core/kernel/src/launch/guard.rs` (the rules are mirrored already); `pkg/problems.rs:315-390` stops running `python3 -I`; repoint `serve/host/src/cards/access_tests.rs:15`, which reads the source of `kith_guard.py`. `kith_guard.py` goes with its last user (P5-3). PORT AS-IS. S. Depends: none. Owner: retire-python-now-guard. Status: in flight.
- [ ] **P1-9. Route `kith-permit` to `kith kernel permit`.** Hooks `denied`/`once` (`launch/settings.rs:994-1002`), the `kithpermit` MCP (`launch/mcp.rs:14`), `issue` and `apply` (`assemble.rs:1458-1476`) and the calls in `bin/kith:1129,1352,1466,1590-1627`; `kernel permit` covers all of it (`core/kernel/src/permit/`) but nothing points at it. Its door and bug halves still shell to `kith_door` and `kith_bug` (`permit/door.rs:25,61`, `permit/bug.rs:26`); those go with P3-12 and P4-17. PORT AS-IS. M. Depends: none. Owner: retire-python-now-permit. Status: in flight.
- [ ] **P1-10. Delete the `kith-cost` stub and rewrite `dev.kith.cost`.** Write down the new plist by hand (`kith-uv run --no-project -- <kith> cost sync`, or the host's own timer), apply it on both Macs, then delete `kernel/bin/kith-cost` and `kernel/tests/kith-cost-stub-check`. DELETE NOW after the plist. S. Depends: P1-4, P1-11. Owner: retire-python-now-cost. Status: in flight.
- [ ] **P1-11. Test that `kith-uv` runs a plain program** with no Python (a Mac without `python3`, `UV_PYTHON_DOWNLOADS=never`). Untested; `kith-uv run --no-project -- <kith> ...` is the shape P1-10 and P1-16 depend on. Do this before anything that rewrites a plist. PORT AS-IS (a test). S. Depends: none. Owner: unowned. Status: open.
- [ ] **P1-12. One run path.** **REDESIGN**, needs architect design first. The terminal calls `kith kernel launch --interactive`, then `kernel threads run`; `bin/kith` keeps nothing but argv. The Rust launch already assembles the same settings (`launch/settings.rs`, `assemble.rs`); missing are the terminal-only parts: the backup-account walk, `--held`, the take-over note and the outbox env. Removes the bash launch body (`:372-1646`) and its inline Python: settings in one `python3 -I -c` (`:964-1170`), facets (`:670`), accounts launch-env (`:714-734`), runenv (`:1421`), uuid (`:1529`), realpath and the stub lookup (`:1544,:1548`). Also makes chat-platform turns and card resumes Rust. Folds in P1-13 and P1-14 so they are not designed twice. L. Depends: P1-7, P1-9. Owner: architect design pending. Status: not started.
- [ ] **P1-13. `kith_settings.py` reads** (permission_mode, reads-others, call_model: `kernel/bin/kith:838,841,1373`) go to `kith kernel settings`, which answers every read; only the `set` verbs are routed today. The plugin's read (`register.tsx:127`) is P4-23. Folds into P1-12 (the bash launch body that reads them goes). PORT AS-IS. S. Depends: P1-12. Owner: with P1-12. Status: not started.
- [ ] **P1-14. `kith_accounts.py` chain verbs** (launch-env, main-env, run-chain, `backups-json`, `accounts-json`: `kernel/bin/kith:342-343,596,609,714-734`; the host's `agent/hq.rs:972-1014`; `kith-pkg`, `kith-scratch`) onto `kith kernel account|chain`. Folds into P1-12. The module itself goes in P5-3 (its last users are `kith-pkg` and `kith-scratch`, P4). PORT AS-IS. M. Depends: P1-12. Owner: with P1-12. Status: not started.
- [ ] **P1-15. `kith-auth mcp-config` and `exec` in process.** **REDESIGN**, needs architect design first. The Rust launch calls `people.mcpConfig` in process (a switched Mac's `kith-auth` already routes it to `people.*` at `:3477-3513`, but as a Python process); shared MCP servers start as `kith kernel secrets exec --for A SVC -- cmd`. This is the seed of "keys never in the agent, one check per call" (decisions 2026-11-21). M. Depends: none. Owner: architect design pending. Status: not started.
- [ ] **P1-16. `adapters/harness.py` through the core's ACP engines.** **REDESIGN**, needs architect design first. One-shots (`bin/kith:1454`) go through `kernel run-once` as the host's turns do (`serve/host/src/runonce.rs`); engines without ACP get a Rust adapter or are dropped. Kai lists which engines he uses. M. Depends: P1-12. Owner: architect design pending. Status: not started.
- [ ] **P1-17. `kith-serve-launch` becomes `kith serve --supervise`.** **REDESIGN**, needs architect design first. The host binary takes the launcher role (child, signal forwarding, 20 s SIGKILL, ported from 60 lines) and starts behind `kith-uv` as a plain program; the `launcher` of `~/.kith/serve/config.json` and the `dev.kith.serve` plist change with it. S. Depends: P1-11. Owner: architect design pending. Status: not started.
- [ ] **P1-18. `kith_readfence.py`.** Delete with the bash launch: only `bin/kith:1306`'s guest fallback names it; the Rust launch always uses `kernel readfence` (`core/kernel/tests/readfence_parity.rs` runs `python3`: a golden replay). DELETE with P1-12. S. Depends: P1-12. Owner: unowned. Status: not started.
- [ ] **P1-19. Delete `kith-sessions`.** Goes when the `KITH_CORE=python` fallbacks go: `bin/kith:1646`, `kith-core.sh:110`, the launch fallback hook (`launch/settings.rs:794`), `kith-auth:2550`; keep `transcripts_preflight.rs:85`'s check of a live old hook until both Macs run the head. `kith kernel threads` is the complete twin. Replace its entries in the `thread-writers` and `writer-lock` models' `FILES`. DELETE. S. Depends: P0-8, P1-12. Owner: unowned. Status: not started.

## Phase 2: core services (wake S4, ask, schedules)

The order matters: the asker, the callee, the daemon tick, a terminal release and a card answer all act in any order, so
**the TLA+ model of S4 comes before anything is proposed** (L1367 lists the actors: host taker, tick drain, terminal
release, waiter fire, card Stop). `dev.kith.sched` is today's wake-queue clock: its minute tick skips the jobs on a switched
Mac but first runs `kith-ask --sweep` and `kith-wake drain` (`kernel/bin/kith-sched:650-655`), and `threads/run.rs:292-307`
runs the same two after every terminal run. It cannot go before S4. Phase 2: **not started; architect S4 design + TLA+ model
in progress.**

- [ ] **P2-1. S4 design and TLA+ model.** The host daemon drains the wake queue and fires waiters from core.db each tick; the host begins a wake turn by its id (as `routines/core_runs.rs` does); the terminal's release enqueues and the daemon delivers; one store, import/export, a cutover module `wake`. Model-check before proposal. REDESIGN, needs architect design first. L. Depends: none. Owner: architect (in progress). Status: design and model in progress.
- [ ] **P2-2. Wake in the core (`kith-wake`, 762 lines).** Drain and waiter firing in the daemon (`core/runtime/src/wake/` has the queue, no `fn drain`); host `wake.rs:63-200` and `cards/resume.rs` (13 sites), `api/people.rs:860`, `bin/kith wake` call the core; retires `run.rs` `wake_owed` and the Python at `run.rs:303`. The host arm of the old R5 ports 3/4 was parked on this. REDESIGN, needs architect design first. L. Depends: P2-1. Owner: architect design pending. Status: not started.
- [ ] **P2-3. Ask on the job ledger (`kith-ask`, 1142 lines).** An ask is a job-ledger row (L1275) over the asks table (`core/runtime/src/asks`, `end_tree`) plus wake; a `--bg` finish wakes the asker; the dead-ask sweep runs in the daemon instead of after every terminal run; "waiting on Kai" shows up the chain; the CLI is a thin Rust client. Callers: `PROTOCOL.md:50`, host `delegation.rs:41,199`, `cards/mod.rs:344`, `cards/resume.rs:1325`, `permit/helper.rs:76`, `run.rs:302`, `kith-sched:650`. Agent teams (2026-11-19 open line) build on the same row. REDESIGN, needs architect design first. L. Depends: P2-2. Owner: architect design pending. Status: not started.
- [ ] **P2-4. The host begins the core-fired schedule turn itself** as a normal core turn with the job's prompt: no `kith-sched run-turn` subprocess (`routines/core_cron.rs:195-209`). Then C6 (host routine runner) follows. REDESIGN, needs architect design first. M. Depends: P2-2. Owner: architect design pending. Status: not started.
- [ ] **P2-5. `kith schedule` verbs as a Rust CLI over `schedule.*`** (ls/add/rm/pause/resume/run/log; the Python already speaks `schedule.*` through `kernel/lib/kith_sched_core.py`); callers `agent/hq.rs:646`, `kith-info:330`. PORT AS-IS. M. Depends: none. Owner: unowned. Status: open.
- [ ] **P2-6. Retire `kith-sched`, `dev.kith.sched`, `kith_sched_core.py`.** Drop `tick`, `--install` (`kith_migrate.py:566` is gone by then) and the launchd job; delete the script and its checks (the cron-table and json cases are already frozen in `golden/sched.json` and `fixtures/routines-next.json`); drop the `schedule-fire` model's `kith-sched` entry. `kith_launchd.py` stays until P4-14. DELETE. S. Depends: P2-3, P2-4, P2-5. Owner: unowned. Status: not started.
- [ ] **P2-7. Delete `kith_claims.py`, `kith_registry.py`, `kith_procstart.py`** with their importers (`kith-sessions`, `kith-wake`, `kith-sched`); `kith-procstart-check` and `core/procstart/tests/parity.rs` become golden. DELETE. S. Depends: P1-19, P2-2, P2-6. Owner: unowned. Status: not started.

## Phase 3: host subprocesses (the host calls the script by path)

- [ ] **P3-1. The host calls `people.*` in process** instead of `kith-auth` by path: `channels/hq.rs:159,165` (who/resolve per chat message), `channels/manager.rs:74,88`, `judge.rs:61`, `rich_card/jev.rs:614`, `cards/access.rs:367`, `cards/edge.rs:185-221`, `cards/secret.rs:144`, `launch_cmd.rs`; kernel callers `cost/views.rs:546,581`, `reports.rs:360`. The host already does it for run_auth (`api/authority.rs:957-992`). PORT AS-IS. L. Depends: P1-15. Owner: unowned. Status: not started.
- [ ] **P3-2. A thin `kith auth` Rust CLI** keeping `get`, `exec`, `keys`, `may`, `resolve` (`kernel secrets may|resolve` exist). Exit-3 shapes with no caller go with their callers (`helper-mcp` with P4-23, `mcp-names` with P3-10). PORT AS-IS. M. Depends: P1-15. Owner: unowned. Status: not started.
- [ ] **P3-3. Room secrets (`once`) as core rows**, if room secrets stay (`cards/room_secret.rs:101-206`). REDESIGN, needs architect design first. M. Depends: P3-1. Owner: architect design pending. Status: not started.
- [ ] **P3-4. Delete `kith-auth`.** Drop the unswitched auth.json code path (importer-only), the router, and `hq-auth`; replace its entries in the `revoke-access`, `secret-cards` models' `FILES` and the `invitee-access` skill; the many `kith-auth-*` checks port to Rust integration tests or go (the tombstone-only ones are deleted); `serve/host/src/api/rig.rs` runs a core instead of the script. DELETE. M. Depends: P3-1, P3-2, P3-3, P1-12, P1-15, P1-9. Owner: unowned. Status: not started.
- [ ] **P3-5. "Always allow" as allowances (`kith-allow`, `kith_allowed.py`).** Always turns on a named allowance and never saves an exact command, so `--derive` is not ported (host `cards/autoallow.rs:44-178`, `bin/kith:169`); needs Kai's pick (option A of the 2026-11-19 open line). REDESIGN, needs architect design first. M. Depends: none. Owner: architect design pending. Status: not started.
- [ ] **P3-6. `kith-cards`** (`--may-read AGENT`, `cards/history.rs:15`) into the host, which owns cards. PORT AS-IS. S. Depends: none. Owner: unowned. Status: not started.
- [ ] **P3-7. `kith-send-to`** (wakes an outside bot guest in a room, `chat/group.rs:797-814`) into the host. PORT AS-IS. S. Depends: P2-2. Owner: unowned. Status: not started.
- [ ] **P3-8. `kith-recall`, `kith_recall.py`, `kith_runsocket.py`** into the host's `kith memory`/`recall` (`kernel.rs:31` already owns the other memory verbs); callers `hqmem/tidy.rs:984`, `agent/hq.rs:904`, `runtools.rs:121-131` (which runs the Python back), `PROTOCOL.md:19`, the kith-rooms skill; update the `run-tokens` skill text. PORT AS-IS. M. Depends: none. Owner: unowned. Status: not started.
- [ ] **P3-9. `kith-memory ls|search`** (owner search; guest branch to the run socket) in the same PR as P3-8. PORT AS-IS. S. Depends: P3-8. Owner: unowned. Status: not started.
- [ ] **P3-10. `kith-info` answered by the host in Rust** from `pkg::resolve`, `schedule.jobs`, cost views, facets and the package: no subprocess, no 30 s cache; the JSON shape stays (`serve/docs/bot-info.md`). Callers `agent/hq.rs:701`, `bin/kith:337`. REDESIGN, needs architect design first. M. Depends: P2-5. Owner: architect design pending. Status: not started.
- [ ] **P3-11. `kith-people`, `kith_people_access.py`, `kith_host.py`.** Route `kith people`/`invite` to `kernel people` (`main.rs:492`, exists unrouted; access goes to core `people.grant|revoke`). PORT AS-IS. S. Depends: none. Owner: unowned. Status: not started.
- [ ] **P3-12. Guest doors, process side (`kith-door`, `kith_door.py`, `kith_door_core.py`, `door-chrome`, `door-preload.mjs`).** **DROP-candidate**: PORT (as a REDESIGN) unless Kai drops it; ask via `kith ask product-lead` when the item comes up (needed only if invited people or bot guests use a door). Redesign: core door rows with basis+gen, the starter or a sweeper confirms "reaped", the box start moves into Rust (`permit/door.rs`); host `doors.rs:47` sweeps every minute; TLC (revoke vs start). Needs architect design first. L. Depends: P1-9. Owner: architect design pending. Status: not started.
- [ ] **P3-13. Host `agent/gate/reclaim.rs:67` runs `python3 <agents>/resource-manager/role/tools/res.py`**: product code hard-wiring one agent's Python tool. REDESIGN into a declared package hook, or DROP. S. Depends: none. Owner: unowned. Status: not started (flag).

## Phase 4: operator tools (parallel, S-M each)

Mostly PORT AS-IS busywork; each ends by deleting its script, so rule 3 and rule 4 apply per PR.

- [ ] **P4-1. `kith-pkg` beyond `resolve`:** list, set-field/set-harness, install/update, pin/unpin/rollback, check, history/restore/gc, deps (`bin/kith:156-162,241,262,307`; host `agent/hq.rs:395,562`, `api/kith_tools.rs`, `api/scratch.rs`; `kith-init`, `kith-doctor`, `kith-state`, `kith-scratch`, `kith-skills`; `--json` forms frozen by `kith-pkg-json-check`). PORT AS-IS. L. Depends: P1-14. Owner: unowned. Status: not started.
- [ ] **P4-2. `kith-pkg` marketplace verbs `share`, `push`, `commit`, `search`, `market`.** **DROP-candidate**: PORT unless Kai drops it (ask via `kith ask product-lead`; the marketplace is Codync-inherited, decisions L900). M. Depends: P4-1. Owner: unowned. Status: not started.
- [ ] **P4-3. `kith-skills`** (add/remove). PORT AS-IS. S. Depends: P4-1. Owner: unowned. Status: not started.
- [ ] **P4-4. `kith-scratch`** (`keep`, host `api/scratch.rs:83`) with `kith_trajectory.py`. PORT AS-IS. S. Depends: P4-1, P1-14. Owner: unowned. Status: not started.
- [ ] **P4-5. `kith-project`** (add/notes/ls; host `agent/hq.rs:455-967`, `PROTOCOL.md:15`). PORT AS-IS. S. Depends: none. Owner: unowned. Status: not started.
- [ ] **P4-6. `kith-group`** (bot guests in rooms) as Rust verbs on the host API. PORT AS-IS. S. Depends: none. Owner: unowned. Status: not started.
- [ ] **P4-7. `kith-connector`**, folded into product-lead's open Composio decision (2026-11-21). PORT AS-IS. S. Depends: none. Owner: unowned. Status: not started.
- [ ] **P4-8. `kith-serve`** (bash; `python3 -I` writes `config.json` `:175,197`) into the host's `serve` verbs. PORT AS-IS. S. Depends: P1-17. Owner: unowned. Status: not started.
- [ ] **P4-9. `kith-send`** (bash with inline Python). Keeps its path as an sh stub until P4-24 moves the two agent tools off it. PORT AS-IS. S. Depends: none. Owner: unowned. Status: not started.
- [ ] **P4-10. `kith-call`** (bash, `python3 -I -` body). PORT AS-IS. S. Depends: none. Owner: unowned. Status: not started.
- [ ] **P4-11. `kith-new`** (scaffold; `kith new` from the host, `agent/hq.rs:611`). PORT AS-IS. S. Depends: none. Owner: unowned. Status: not started.
- [ ] **P4-12. `kith-doctor`** (610 lines) rewritten for the Rust world: cutover status, launchd plists naming kernel scripts (it warns about them today), program version, and "not switched" naming (replaces the dropped start-up refusal). PORT AS-IS. M. Depends: none. Owner: unowned. Status: not started.
- [ ] **P4-13. `kith-init`** (526 lines; a hard Python dependency of a full install, `install.sh:115`). PORT AS-IS. M. Depends: P4-1. Owner: unowned. Status: not started.
- [ ] **P4-14. `kith-state` and `dev.kith.backup`:** `kith backup` in Rust (git add/commit/push of the state repos); the release rewrites the plist in the same step; `kith_launchd.py` goes with it. Live nightly on Kai's Mac. PORT AS-IS. S. Depends: P1-11. Owner: unowned. Status: not started.
- [ ] **P4-15. `kith-self-update`** folded into the host updater (`serve/host/src/update/`) plus `install.sh`. REDESIGN, needs architect design first. S. Depends: none. Owner: architect design pending. Status: not started.
- [ ] **P4-16. `kith-keys`, `kith-who`.** PROTOCOL.md and the skills name them. PORT AS-IS. S each. Depends: P3-2 (keys). Owner: unowned. Status: not started.
- [ ] **P4-17. `kith-bug`, `kith_bug.py`, `kith_redact.py`.** PORT AS-IS. S. Depends: P1-9. Owner: unowned. Status: not started.
- [ ] **P4-18. `kith-chain`, `kith_chain.py`** (may later sit on the job ledger). PORT AS-IS. S. Depends: none. Owner: unowned. Status: not started.
- [ ] **P4-19. `kith-helper-allow`** with the plugin. PORT AS-IS. S. Depends: P4-23. Owner: unowned. Status: not started.
- [ ] **P4-20. `kith-remote`** (`agent@mini` over ssh; successor is the Mac link, not built). **DROP-candidate**: PORT unless Kai drops it (ask: still used?). S. Depends: P2-3 (it loads `kith-ask`). Owner: unowned. Status: not started.
- [ ] **P4-21. `kith-acp` and `adapters/acp/*` (Python shim).** **DROP-candidate**: PORT unless Kai drops it (ask: does he use an outside ACP client such as Zed to reach kith agents?). S. Depends: none. Owner: unowned. Status: not started.
- [ ] **P4-22. hq-to-kith rename steps of `kith_migrate.py`.** **DROP-candidate**: PORT unless Kai drops it; the natural moment is once both Macs report migrated (this Mac has `~/.hq` as a link only). The forwarder goes in P0-7. S. Depends: P0-7. Owner: unowned. Status: not started.
- [ ] **P4-23. Claude Code plugin `register.tsx`** calls `kith kernel ...` instead of Python: `kith_paths --json` (`:59`), `kith-pkg list|resolve --all` (`:70,:81`), `kith_settings` (`:127`), `kith_facets load` (`:325,:348`), `kith-auth helper-mcp|may|resolve` (`:121,:292,:339`), `kith-helper-allow` (`:135`). Adds `kernel facets load` and `kernel permit helper-mcp|helper-allow` (no CLI verb today). `kith-hooks.test.ts` stays. PORT AS-IS. M. Depends: P1-7, P1-13, P4-1. Owner: unowned. Status: not started.
- [ ] **P4-24. Move the two agent tools that name `kernel/bin/kith-send`** (resource-manager `watch.py:17`, devops `uptime.py:40`) to `kith send`; then the P4-9 stub can go. These are agents' own tools, edited in their role repos. PORT AS-IS. S. Depends: P4-9. Owner: unowned (agents' owners). Status: not started.

## Phase 5: seams (the end of the Python kernel)

- [ ] **P5-1. Rust route table replaces the bash dispatch of `bin/kith`** (`serve/host/src/kernel.rs` `route()` owns argv0; every `kith <verb>` answered in Rust). REDESIGN, needs architect design first. M. Depends: P1-12, P3-4, P4-1..P4-23 (every verb ported or dropped). Owner: architect design pending. Status: not started.
- [ ] **P5-2. Delete `bin/kith`, `kernel/lib/kith-core.sh`, `kernel/lib/core-switched.txt`** and the remaining `KITH_CORE=python` branches: nothing is left to switch back to. DELETE. S. Depends: P5-1. Owner: unowned. Status: not started.
- [ ] **P5-3. Delete the last `kernel/lib/*.py`:** `kith_facets`, `kith_jobs`, `kith_runenv`, `kith_trajectory`, `kith_allowed`, `kith_guard`, `kith_settings`, `kith_accounts`, `kith_paths`, `kith_redact`, `kith_launchd` as each loses its last user (rule 3), `kith_tombstone` last. DELETE. S. Depends: P5-1, P4-4, P4-14, P4-17. Owner: unowned. Status: not started.
- [ ] **P5-4. `scripts/stage-kernel.sh` stops copying `kernel/bin` and `kernel/lib`;** the data they hold (`starters.json`, `user.md`, `agents.gitignore`, `dot-kith.gitignore`, `kernel/skills/`, `kernel/services/*.json`, `PROTOCOL.md`, `kernel/agents/scratch`) moves to `share/kith/`; `apps/desktop/tools/native-host.mjs` follows. PORT AS-IS. M. Depends: P5-2, P5-3. Owner: unowned. Status: not started.
- [ ] **P5-5. Drop `python@3`** from the Homebrew formula (`packaging/homebrew/kith.rb`) and the installer's `python3` calls (`scripts/install.sh`: `quiesce.py`, `kith_paths.py` for `auth.json` locks; `install.sh`). The installer's gate runs the new program, not Python. PORT AS-IS. S. Depends: P5-3. Owner: unowned. Status: not started.
- [ ] **P5-6. Raise `MIN_CLI`** (`serve/host/src/kernel.rs:172`) to the first Python-free release. PORT AS-IS. S. Depends: P5-5. Owner: unowned. Status: not started.
- [ ] **P5-7. Delete the stubs** `kernel/bin/kith-room-ledger` and whatever stubs remain, except `project-id` (agents and PROTOCOL.md run it by name) and `kith-send` until P4-24. DELETE. S. Depends: P5-2. Owner: unowned. Status: not started.
- [ ] **P5-8. Test and CI tail:** `kith-core-boundary-check` rewritten to forbid new `python3` spawns (not Python importing Rust); `kith-difftest` runs `--golden` only; Rust parity tests that run `python3` (`core/kernel/tests/schedule_parity.rs`, `core/runtime/src/import/tests.rs`, `authority/tests/revoke.rs`, `core/kernel/src/secrets/tests.rs`) become golden replays; `kith doctor` reports no plist naming a kernel script. PORT AS-IS. M. Depends: P5-2. Owner: unowned. Status: not started.

## Phase 6: importers

- [ ] **P6-1. Delete `core/runtime/src/import/*`** (accounts, asks, authority, claims, schedules, transcripts, wake) and the `export_back` halves of the modules, once both Macs have stepped every module (no two-release floor). `kith rollback MODULE` ends with them. DELETE. M. Depends: P0-10, P5-2. Owner: unowned. Status: not started.
- [ ] **P6-2. Delete the tombstone readers in Rust** and `cutover` code that only the importers used. DELETE. S. Depends: P6-1. Owner: unowned. Status: not started.

## 1. How the two sides fit together (mechanisms)

Three mechanisms exist; the series reuses them and invents little.

| Mechanism | Where | What it does | Limit |
|---|---|---|---|
| Verb switch | `kernel/lib/core-switched.txt`, read by `kernel_core_exec` in `kernel/lib/kith-core.sh` | A line `verb<TAB>module[<TAB>cutover module]` sends `kith VERB` to `kith kernel VERB` when this Mac's `kith` program lists the module in `kith kernel version`; otherwise the Python answers, silently. Removing the line is the rollback. | The fallback is silent; rollback needs the Python still there. Goes in P5-2. |
| Cutover module | `core/runtime/src/cutover/` (`kith migrate step|status|check|rollback MODULE`); registry in `cutover/mod.rs`: `accounts`, `transcripts`, `claims`, `schedules`, `authority` | Moves a Python-owned store into core.db, checks the answers match, writes `<machine>/core/cutover/MODULE.switch`, replaces the source files with tombstones; `rollback` writes them back. | Per Mac and deliberate ("never automatic"). `kith migrate` dispatches through Python until P0-7. |
| Tombstone | `core/runtime/src/cutover/tombstone.rs` writes, `kernel/lib/kith_tombstone.py` reads | A Python reader that meets one refuses (`Tombstoned`, exit 3 or 4); `KITH_CORE=python` is refused for a switched module. | Per file or module, not per verb. |

Also true:

- The Python files travel in every package: `scripts/stage-kernel.sh` copies `kernel/` (minus `tests/`) into the release
  tarball, the Homebrew formula and the Mac app (`apps/desktop/tools/native-host.mjs`). A release installs as
  `<prefix>/cli/<version>/`, `cli/current` points at it, and the last `KITH_KEEP_VERSIONS` (default 3) stay
  (`scripts/install.sh`). **Deletion happens in a new release folder, never in place**; going back is "install the previous
  folder", which works only while a deletion release adds no core.db migration (an older binary refuses to write a core.db
  with a step it does not know, `core/runtime/src/db.rs:421-430`): P0-12.
- The apps do not run Python. The Rust host and the Rust launch do, in a few places: `permit/door.rs`, `pkg/problems.rs`
  (`kith_guard`), the host's `kith_paths.py --json` calls, and the Python the launch asks (`kith-auth mcp-config`). The host
  spawns kernel scripts by path or through `kith` (`serve/host/src/kernel.rs`, `Route::Kernel` hands every verb it does not
  own to `kernel/bin/kith`). The desktop and iOS apps call the host's API only.
- A stub is an `sh` file that finds `kith` at a fixed relative path (no `kith_paths.py`) and execs `kith kernel <verb> "$@"`.
  It serves a caller that executes the file (an old host's `Command::new`, a hook command). It does **not** serve a caller
  that names an interpreter (`kith-uv run --script <file>`, `python3 <path>`): those are rewritten before the delete.

## 2. Live state (2026-11-21)

- **This Mac (Kai's):** `kith kernel cutover status` reports accounts, transcripts, claims, schedules and authority all
  switched. Installed CLI `0.1.4-rust.02a1921` (2026-11-19), behind the head (P0-9).
- **launchd jobs here** (`~/Library/LaunchAgents`, ProgramArguments read):
  - `dev.kith.serve`: `kith-uv run --quiet --script <cli/current>/kernel/bin/kith-serve-launch <kith> serve --port 19224`;
    the launcher comes from `~/.kith/serve/config.json` "launcher" (P1-17).
  - `dev.kith.sched`: `kith-uv ... kith-sched tick`. **Installed, and today's wake-queue clock**: it skips the jobs
    ("tick skipped, the schedules moved to the Kith core", `~/.kith/logs/sched/tick.log`) but runs `kith-ask --sweep` and
    `kith-wake drain` first. It cannot go before S4 (P2-6). (The first plan's line "Kai's Mac has no `dev.kith.sched`
    installed" was out of date.)
  - `dev.kith.cost`: `kith-uv ... kith-cost sync`; the log is healthy (P1-10).
  - `dev.kith.backup`: `kith-uv ... kith-state backup --yes` (P4-14).
  - Agents' own jobs: `dev.kith.house` (house.py), `dev.kith.monitor` and `dev.kith.uptime` (devops). Their Python is the
    agents', outside this series, but they need `kith-uv`/uv (section 4).
- **MacBookPro:** cutover state and branch unknown (P0-10); L1381 (its host-native bot count) is open.
- **What every run still runs in Python today** (a switched Mac, head build; evidence in the gap analysis):
  - `kith` calls `python3 kith_paths.py --sh` first, and the router needs Python to find the Rust program (P1-7).
  - The interactive launch is the whole bash+Python body; only `--launch-json` execs `kernel launch` (P1-12).
  - The Rust launch runs `kith-auth mcp-config`, and every shared MCP server starts as `kith-auth exec` (P1-15).
  - Hooks written into each run's settings: `kith-permit denied|once` and the `kithpermit` MCP server (P1-9). Hooks are
    fresh at each launch, but a run already going keeps the command line it started with, which is why a moved hook leaves a
    stub behind until sessions end.
  - After each terminal run, `kith kernel threads run` starts `kith-ask --sweep` and `kith-wake drain` (P2).
  - The host's subprocesses by path: `kith-wake`, `kith-door`, `kith-auth` (the busiest), `kith-allow`, `kith-cards`,
    `kith-send-to`, `kith-recall`, `kith-sched run-turn`, `kith-info` (Phase 2 and 3).
  - `dev.kith.serve`, `sched`, `backup` and `cost` start Python (Phase 1, 2, 4).

Counted roughly in the gap analysis: 7 L, about 12 M, about 25 S. Phases 1 to 3 want Opus; Phase 4 is parallel busywork for
Sonnet coders.

## 3. Order

```
P0 ──► P1 ──► P5 ──► P6
 │      │      ▲
 │      └──► P3┘      P3 also needs P2
 ├──► P2 ──► P3
 └──► P4 ────────────► P5
```

Phase 1 comes first because it removes every per-launch Python start and makes chat and card-resume turns Rust. Phase 2 is
order-dependent and starts only after the S4 model. Phase 4 is independent and can run beside Phases 1 to 3; P4-23 (the
plugin) needs P1-7 and P1-13.

## 4. `kith-uv` and agents' own Python (they stay)

Agents' tools live in their own role repos (`agents/<name>/role/tools/`); `kernel/agents/` holds only `scratch`. House's
`house.py`, devops' `monitor.py` and `uptime.py` and resource-manager's `log_cost.py` need a Python (often uv's script
dependency resolution), not the kernel. Retiring the kernel's Python does not touch them.

`kith-uv` (`~/.kith/bin/kith-uv`, found by `kith_paths.launcher()`) is a copy of `uv` that holds Full Disk Access, because
launchd credits file access to the program it starts and a repo on an external disk (Kai's) needs a launcher whose path and
signature do not change when kith updates (inferred; `docs/moving-from-hq.md` records "Full Disk Access is per path").
**Decision (2026-11-17, unchanged): keep `kith-uv`** as "the Full Disk Access launcher agents' tools run behind", not as a
kernel dependency; the kernel's own jobs run behind it as a plain program (`kith-uv run --no-project -- <kith> <verb>`).
Never rename or replace it (a non-uv binary has a new signature and needs a new grant; removing it breaks every agent's
plist and the desktop app's "started by kith" test, `host-update.test.ts:120`). **Untested:** that uv runs a plain program
with no Python on a Mac without `python3` and with `UV_PYTHON_DOWNLOADS=never` (P1-11, before any plist is rewritten).

Python that is not the kernel and stays: `scripts/voice-samples/`, `apps/ios/tools/`, `apps/tests/`,
`serve/host/tests/fixtures/*.py`, the difftest `*-old.py` frozen references and `kernel/tests/kith-auth-props.py` (a property
test: port to proptest or keep as a golden replay when `kith-auth` goes).

## 5. Tests and model checks, per deleting PR

Rule 4 in practice. A deleted check comes out of `ci.yml` and `ci-exempt.txt` in the same PR (`kith-ci-names-check`).

- **Switch checks** (`kith-cost-switch-check`, `kith-settings-switch-check`, `kith-account-state-switch-check`,
  `kith-pkg-pid-switch-check`, `kith-limit-check`, `kith-run-once-cost-check`) compare Python to Rust; with the Python gone
  there is nothing to compare, so they are deleted and the difftest golden carries the answer.
- **Corpora to freeze before their script goes** where `kernel/tests/difftest/golden/` lacks one: `json-misc`, `json-pkg`,
  `json-sched`, `launch*`, `paths*`, `run-once`. The `*-old.py` files stay.
- **Checks that exercise a script** port to Rust tests (`core/runtime` tests, `serve/host` `api/*_tests.rs`, host e2e for the
  launchd and sandbox ones) or are deleted when they only prove "Python refuses a tombstone".
- **Models and skills whose drift list names Python:** `thread-writers` and `writer-lock` (`bin/kith`, `kith-lifecycle`,
  `kith-sessions`, `kith-wake`, `lib/kith_claims.py`, `lib/kith-core.sh`: P0-8, P1-19, P2-2, P2-7, P5-2); `revoke-access`
  (`kith-auth`, `bin/kith`, `lib/kith_door.py`, `kith-permit ask_helper`: P1-9, P3-4, P3-12); `secret-cards` (`kith-auth`:
  P3-4); `schedule-fire` (`kith-sched`: P2-6); `invitee-access` (skill; P3-4); `run-tokens` (skill: `kith-recall`,
  `kith_runsocket.py`, `kith-memory`, the `project-id` guest branch: P3-8). `holder-lease` and `rooms-with-people` name no
  Python. A path left in a CI `changes` filter is harmless, but a path left in `drift.sh` `FILES` makes the drift check
  report "file missing": read each `drift.sh` before the step and re-run the model in the same PR.
- `host-kernel-tests.txt` and `scripts/ci/host-kernel-tests.sh check`: host tests that run the real `kith-auth` through the
  api `Rig` move to a core in P3-4.

## 6. Packaging, installers, launchd

| Piece | Python or uv in it | Effect |
|---|---|---|
| Release tarball, Mac app, Homebrew | `stage-kernel.sh` copies all of `kernel/`; the formula has `depends_on "python@3"` | smaller as scripts go; the last step is P5-4 and P5-5 (a clean Mac's `python3` is the Xcode stub popup, so dropping it is a real gain) |
| `scripts/install.sh`, `install.sh` | `python3` for the atomic swap and drain (`quiesce.py`), `kith_paths.py` for `auth.json` locks | the installer is the gate for a Python-free release and must not need Python to install one (P5-5) |
| `kith-self-update` | drives `install.sh`, brew, git | P4-15 |
| Desktop host service | `apps/desktop/src/shared/host-service.ts`, `main/host-controller.ts`: treats a plist that starts `kith-uv` as one `kith serve start` wrote | stays; `ensureCli` installs the bundled CLI when below `declared.cli` (`apps/desktop/package.json` `kith.cli`) |
| launchd plists | a plist names a script under `cli/current/share/kith/kernel/bin/`, so a deleted script is a job that fails every tick | each job is rewritten before its script goes: serve (P1-17), cost (P1-10), sched (P2-6), backup (P4-14); `kith doctor` lists any plist that names a kernel script |

## 7. Edge cases

- **States.** A cutover module is unswitched, switched or rolled back on a Mac; a release that deletes Python is only for a
  Mac where every module is switched (rule 2, the installer check). Kai's Mac is switched; the MacBookPro is unknown.
- **Existing features.** Hooks written by the launch change in the launch's Rust one step at a time; a run in flight keeps
  its old command, which is a stub that forwards. Agents' own `.claude/settings.json` or `role/settings.json` may name
  `kith-*`: grep `agents/*/role/settings.json` on each Mac before a script goes. `hq-*` links are symlinks to `kith-*` and
  go with the hq rename work (P4-22). `apps/desktop` and iOS are unaffected by the kernel steps.
- **Repeats.** Rule 5. The cutover framework's `step`, `rollback` and `status` are idempotent under the core.db lifetime
  lock.
- **Old data.** `state.json`, claim files, sched `state.json`, `auth.json`, `live/ask/*.reg`, wake queue files and the
  session registry are read by `core/runtime/src/import/*`; those importers outlive every Python writer and go in P6.
- **Invitees and old app builds.** Invitees run no kernel; they use the owner's host through the app or web client. Old
  desktop builds bundle an old kernel and host that call the scripts by path: Kai controls his app version.
- **Windows.** Removing the Python from the run path is what lets an owner run on Windows ("one-bot-model" M5). Out of
  scope here; P1 and P2 are the prerequisite.

## 8. Merging rust-rebuild into main

Separate from this series and tracked by main (the gap analysis, section 5): main is an ancestor of rust-rebuild, so the
merge is a fast-forward; CI ran green on the exact tree; the open items are CI on the PR to main, the member web deploy that
fires on `apps/desktop/**`, retargeting the open PRs based on rust-rebuild before deleting that branch, Macs on a main
checkout (read the MacBookPro first, P0-10), pointing flow at main, and the `python-kernel-final` tag (P0-11).

## 9. Superseded: the first plan's R1-R10

| Old step | Now | Old step | Now |
|---|---|---|---|
| R1 leaves with Rust twins | P0-3 (done) | R6 `schedules` | P2-4, P2-5, P2-6 |
| R2 stateless stores (cost, project-id, room-ledger, pkg resolve) | P1-1 to P1-4 (done), P1-10 | R7 the run path | P1-12 to P1-18, P2-3, P3-8, P3-9, P4-21 |
| R3 `accounts` | P0-4 (done), P0-6, P1-14 | R8 operator tools | Phase 4 |
| R4 `authority` | P1-9, P1-15, P3-1 to P3-5, P3-12, P4-17 | R9 the seams | Phase 5, plus P0-7 and P1-17 |
| R5 `claims`, `transcripts` | P0-8, P1-5, P1-6, P1-19, P2-2, P2-7 (ports 1/4, 2/4, 4/4 done; 3/4 parked, see log) | R10 importers | Phase 6 |

Old text that code comments still cite: "rule 3" (the stub rule) is section 1's last bullet; "section 3.1" (the window and the
three-layer gate) is the dropped Gates above; "R9" means P5.

## Log

- 2026-11-17: first plan (R1-R10) written and architect-reviewed in five rounds (#1025). Verdict: approved; the schema rule,
  the interpreter-callers rule, the import-graph rule 5 and the mechanical file map came out of it.
- 2026-11-19: Step 0 (#1050): `status MODULE` (exit 0 only when switched), `status --json`, `status --require FILE` (a module
  counts as switched only when the record says `switched` and every tombstone is in place; a damaged record or an
  unfinished step is "no", with the step to run); `kernel/lib/requires-switched.txt` (no module required yet;
  `kernel/tests/kith-requires-switched-check`, in CI); `scripts/py-file-map.py`. Left over and now dropped: the
  loader-to-subprocess prep; still open: the installer must run the **new** program's `--require` (an older program answers
  with a usage error and exit 1, which would block every update).
- 2026-11-19: R2 `project-id` (#1077): the guest branch is `core/kernel/src/project/guest.rs` (a run-socket client speaking
  `kernel/lib/kith_runsocket.py`'s protocol, answered before the registry is opened); `kernel/bin/project-id` is an sh stub
  that execs `kith kernel project-id` on the first program whose `kernel version` lists `project-guest` (exit 3 and one line
  when none); the `project-id` line of `core-switched.txt` is gone; the host answers a guest's `--info` in process; a guest
  run gets `KITH_BIN` and read access to that program (`launch/settings.rs` `link_chain`). `kith_runsocket.py` stays until
  `kith-memory` is ported (P3-8).
- 2026-11-19: R2 `cost` port (#1073): every `kith kernel cost` verb in Rust (`tag`, `record`, the text reports, bare
  `kith cost`); the `cost` line of `core-switched.txt` and `kith_cost_ported` are gone; the bash launch's SessionStart hook
  is `kith kernel cost tag` on a program with `cost-ledger`, the stub otherwise (`kith_cost_bin`); `spent` and `limit` in the
  Rust launch never run the stub; `KITH_CORE=python` changes nothing. Without a program the bash launch skips an owner's
  at-limit hold; a guest is refused.
- 2026-11-19: R2 room-ledger + pkg (#1080): both launch paths write `<kith> kernel room-ledger --kernel K pre|post` when the
  `room-ledger` line is present and the program lists the module, else the stub path; `bin/kith-room-ledger` is a stub (no
  program: silent exit 0, but a `pre` for a room note is denied); `pkg resolve` has no Python fallback (no program: the live
  folder runs, with a line saying why).
- 2026-11-19: R3 PR-B (#1081): `bin/kith-account` is `exec bin/kith account "$@"` (no program: one line, exit 1, also for
  `KITH_CORE=python`); the `account-state` and `kith backup` Python fallbacks in `bin/kith` are gone; the account difftest
  corpora are golden-only. R1 (#1059): `kith_engines.py`, `hq_settings.py` and the Python read-fence fallback deleted.
- 2026-11-19: R5 ports 1/4 (#1083): every `python3 "$KITH/bin/kith-sessions"` call of `bin/kith` and `kith-core.sh` except
  `run` (`epoch`, `pick`, `latest`, `resolve`, `proc`, `claim`, `terminal`, `release`) and the SessionStart `record` hook go
  to `kith kernel threads` when the line `threads<TAB>threads-writer` is in `core-switched.txt` and the program lists the
  module (decided once per launch, variable `kthreads`; `KITH_CORE=python` is the per-launch rollback). The Rust verbs answer
  both claim modes (Files and Writer, through `claim.*` and `session.record`).
- 2026-11-20: R5 ports 2/4 (#1090): `kith-sessions run` is `kith kernel threads run AGENT -- [kith-stub ...] claude ARGS...`
  (`core/kernel/src/threads/run.rs`, process half `runproc.rs`; module `threads-run`). It forks and waits as the Python did
  (the shell's pid keeps the terminal's claim, claude is its child) and ports the take-over and resume notes, the idle stop
  (idle_since, `KITH_SLEEP_MINUTES`, outstanding work, detached children, the keystroke re-check), the stub's status pipe
  (busy is 75, a lock error is 70), the quit intent, `release_claims` in both modes, the sleep sweep and `wake_owed`.
  `bin/kith`'s last exec uses it only when `kthreads` is set and the program lists `threads-run`.
- 2026-11-20: R5 ports 3/4 (#1084): host `wake.rs` port parked, not ported. Architect: the core stores the wake queue but has
  no drain, no waiter firing and no wake module (S4), and `wake.take` already writes a turn row the host's own `resume.rs`
  start would duplicate; every Python caller writes the files, so a host-only Core arm would split the queue across two
  stores. The host arm is written after the S4 design is model-checked (now P2-1, P2-2).
- 2026-11-21: R5 ports 4/4 (#1086): `kith-lifecycle` (`sweep`, `status`, `intent`) is `kith kernel lifecycle`
  (`core/kernel/src/lifecycle/`, module `lifecycle`); `bin/kith-lifecycle` forwards to it when the `lifecycle` line is in
  `core-switched.txt` and the program lists the module; compaction and end take the writer claim through the shared
  `threads::writer::Taker`. The script and `lib/kith_lifecycle.py` stay until P0-8.
- 2026-11-21: `kernel/bin/kith-cost` becomes a forwarding stub, valid as sh and as Python, so the hand-written `dev.kith.cost`
  plist keeps working (launchd -> kith-uv -> python -> sh -> kith) (#1098; with #1084 in batch #1104). `kith doctor` reports
  any plist that names a kernel script; `kernel/tests/kith-cost-stub-check` holds the file to both languages.
- 2026-11-21: Kai asked what stands between us and retiring Python now; the architect's gap analysis (outside the repo, see
  the top) answered. Kai chose Phase 0 + 1 now, a tracked series like the C series, and "Not sure" on every DROP-candidate:
  nothing is dropped now. Kai-only gates decided (section "Gates"). This file rewritten as P0-P6 (docs only).
