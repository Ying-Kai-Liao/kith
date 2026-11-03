# Plan: one bot model, and a kernel that can carry it

Status: **in progress** (started 2026-11-03). Update the checkboxes and the log at the bottom as pieces land.
Survey this plan is built on: [cli-host-gap-map.md](cli-host-gap-map.md).

## Decisions

- **Every bot is a kith agent.** The host's second kind of bot (a database row with `hq_agent` empty, launched by the
  host itself) is retired. Kith is CLI-first: nothing core is app-only, and nothing a bot can do needs to live in the
  host. Settled 2026-11-03 in the kith project's `decisions.md`.
- **Quick, throwaway chats go to one built-in agent, not to new agents.** A `scratch` agent ships with kith; a quick
  chat is a new thread with it (engine and model picked per thread). "Keep this as an agent" turns a thread into a
  real agent with `kith new`. No folder, repo or GitHub backup per experiment.
- **A fresh core, not a rewrite of everything.** The kernel is redesigned in Rust as the one core (one agent model,
  one secret store, one scheduler, one set of folder rules, engines over ACP), a crate the CLI and the host both link.
  It is designed fresh, not ported line by line. The host becomes a thin server over it; the apps, cloud and relay are
  kept, since they talk to the same host API. The CLI's commands and their `--json` stay the contract. It is built
  **beside** the current kernel, in a top-level `core/` folder of this repo (a Cargo workspace member the host links),
  not in a new repo: the harness needs the old kernel and the new core at the same commit, and the host links the
  crate. Nothing switches on a set day: each command moves when it is proven (see Switching over). Python keeps
  the parts that are still changing fast (packages, agent settings, people) until they settle. Settled 2026-11-03,
  instead of rebuilding the whole app.
- **Same CLI line, same result.** The new core is verified by running the same CLI lines against the old kernel and
  the new one and comparing (see Verification). The model-checked safety specs stay for what that can't see (races).
- **Engines (ACP) move into the kernel.** Detection and ACP launch for every engine, today in
  `serve/host/src/agent/backends.rs`, become the kernel crate's first big module. The CLI and the host ask the kernel
  how to run any engine.

## Why

The host is about 4x the kernel (132k lines of Rust vs 29k of Python and Bash). Features landed in the host first, so
a kith agent has half of what a bot can do and a host bot has the other half:

| | Kith agent (`hq_agent` set) | Host-native bot (`hq_agent` empty) |
|---|---|---|
| Lives in | `agents/<name>/role` (git repo) + memory, data, files | a host database row |
| Starts from | `kith <agent> --launch-json` | the host's own launch (`agent/backends.rs`) |
| Engines | 8 in the kernel; the app path takes claude and codex only | 27 over ACP |
| Tools | own skills, `tools/`, `kith auth grant` services | Marketplace connectors and skills, computer server |
| Memory | kith memory and recall | host chat memory |
| Secrets | `kith-auth` (auth.json + Keychain) | host vault (`market/vault.rs`) |
| Schedules | `kith-sched` (jobs.json) | host routines (`routines/`) |
| Versions, sharing, terminal, runs without the app | yes | no |

On the owner's Mac on 2026-11-03 all 27 active bots (21 own, 6 guests') were already kith agents, so retiring host
bots moves no user data. The work is giving kith agents what host bots could do, then deleting the host path.

## Known limits and what answers them

| Limit | Answer |
|---|---|
| The Python/Bash kernel won't run on Windows; host bots were the only kind that could | Rust kernel (track A). Releases ship macOS only today, so nobody loses anything now |
| An app-made bot becoming a full agent is heavy for an experiment; `kith backup` makes a GitHub repo per agent | Built-in `scratch` agent (C1) |
| Every bot now depends on the kernel's environment (Python, uv), which already breaks on `brew upgrade uv` | Rust kernel; meanwhile a clear "kernel unavailable" state and `kith doctor` checks in the app (C2) |
| Two launch styles per engine (one-shot adapters in `adapters/harness.py`, ACP in the host) | Engines move into the kernel crate with both styles (A2) |
| Launch overhead: `kith <agent> --launch-json` takes ~0.66 s, spread over ~3,000 Bash lines and 15 Python processes | Ported to Rust (A3). Interim if it matters first: one Python process for the whole plan, cached on unchanged inputs |
| Host memory has features the app shows (thread and row summaries, shared project memory) | Parity reached (C4): summaries, delete a file, forget a fact work on kith memory; pin, archive, clear, add entry, the per-bot shared switch and promotions are deliberately not carried over (shared project memory is the kith user layer) |
| Settings edited in the app become package commits, so they apply on every Mac | Per-Mac choices go in kith settings, like permission mode already does (B1) |

- **The core runtime design is agreed** (Kai, 2026-11-03: "The core runtime design is agreed. It changes the order of the
  plan you're following."). [runtime-design.md](runtime-design.md): one daemon with the core inside and a holder below;
  kith owns terminals but no session depends on one; the core holds conversations; Windows matters now. A4-A7 are
  replaced by M0-M5. The pure rules already in the crate (paths, engines, launch, delegation limits, claim resolve)
  become functions inside the core module, not separate switched verbs.

## Tracks

Three tracks. A and B can run in parallel; C needs pieces of both. Each step is one PR (or a few), with a CLI command
and `--json` first and the app calling it after.

### A. The core (Rust crate, designed fresh)

- [x] **A0. Core design.** A short design doc (`docs/plans/core-design.md`): the agent model, secrets, scheduler,
  folders, engines, how the host and the CLI sit on the crate, the `core/` layout, and how `kith`'s dispatcher routes a
  command to the core. Reviewed by the architect agent before A1's code.
- [x] **A0b. Differential harness.** The verification below, as a test runner, before the first module moves.
  `kernel/tests/difftest/` (README there); run it with `kernel/tests/kith-difftest-check`.
- [x] **A1. Folder rules.** One Rust implementation of where kith's folders are (`kernel/lib/kith_paths.py`,
  `serve/host/src/kernel.rs::machine_dir`, `hqmem::Layout::from_env` disagree today: `.migrated` is respected by two
  and not the third). New crate under `serve/` (name it in the PR), linked by the host, exposed to Python through the
  `kith` binary the way `kith memory check` already is. `kith_paths.py` asks it, or is checked against it in CI.
  Done when: one implementation, the three callers agree, a test proves it. Gap map #6; architect ranked it first.
- [x] **A2. Engines and ACP.** Move engine detection and ACP launch from `agent/backends.rs` into the crate. The kernel's
  harness list becomes the crate's list (today 8 in `adapters/harness.py` vs 27 in the host). An engine is the program
  that runs a bot's turns (Claude Code, Codex, Gemini CLI...), not a bot. The host drives all 27 through ACP, each one a
  table entry (command, install, sign-in), so this is one table and one launcher. One-shot runs move to ACP too, so
  `adapters/harness.py`'s per-engine code (8 hand-written paths) goes away instead of growing to 27. `--launch-json` works for
  every engine, not just claude and codex (`kernel/bin/kith`, the `launch` branch). Done when: a kith agent with
  `harness: gemini` (or any listed engine) runs in the app and the terminal.
- [x] **A3. The launch path.** `kith <agent> --launch-json` (prompt assembly, settings, MCP config, accounts) in Rust.
  Target: under 50 ms.
- [ ] **A4. Writer claims and delegation limits.** **Stopped 2026-11-03** (replaced by M0-M5 below). Done: delegation
  limits in the crate (A4-1, #650) and the thread claim resolve logic in the crate (A4-2, #652), neither switched. Per
  Kai, `threads` is not switched and the host is not moved onto file-based claim verbs, because claims move into
  `core.db` in M3.
- [ ] **M0. Daemon front for the core.** Holder skeleton, platform interface, method table, principals, embedded mode,
  fencing; a Windows CI job from day one. Gets: the switched verbs through one API, and a restart that keeps a test
  process. ([runtime-design.md](runtime-design.md) section 7)
  Steps, each with its check (crates and decisions: "Execution additions" in runtime-design.md; per-step rules: [M0 build rules](runtime-design.md#m0-build-rules)):
  - M0a: `core.db`, lock, generation, claim-tx gate, embedded mode, perms and missing-db refusals, 9b rules 1-2,
    boundary check, Windows CI job. Check: switched verbs via embedded core, difftest 100%.
  - M0b (done 2026-11-03, `core/api`; types follow-up open): method table, CLI args + OpenRPC first, types follow-up, seeded from `contract.json` and the B6 `--json`
    shapes; `released.json` + break test.
  - M0c (done 2026-11-03, `core/runtime` daemon + `serve/host/src/core_front.rs`): daemon front, owner socket, version hello. Check: a difftest mode via daemon and via embedded; and until M1 a missing or crashing holder stops neither `kith serve` nor the CLI, and the release bundles `kith-holder` or the daemon runs without it.
  - M0d: `kith-holder` from herdr b63ecbd. Check: spawn/kill/reap, fencing + secret, restart keeps a test process.
    Writer lock ([runtime-design.md](runtime-design.md) section 4): one lock file per conversation in the lock
    folder (internal disk next to `core.db`, 0700), never deleted, separate from today's short-held
    `<sid>.claim.lock`; flock on Unix, LockFileEx on the same path on Windows. For every engine the holder spawns,
    kith's start stub takes it (fd CLOEXEC, so the engine's children never inherit it) before the engine starts,
    starts the engine as its child (never exec) and holds the lock to the engine's exit. Check:
    a second taker gets busy; the holder keeps it across a core kill. #663 merged without it: #669 or a follow-up
    takes it.
  - M0e (done 2026-11-04, `core/runtime` runs.rs + runsock.rs): run socket and token table, `host.sock` not switched. Check: `run-tokens` re-run.
  - M0f: networked holder over SSH stdio, pairing key, reconnect/resume, generation across, lease.
  Before each switch, freeze its difftest cases as golden before Python goes (oracle rule in runtime-design.md).
- [ ] **M1. Holder for terminals, opt-in.** `kith <agent> --held`, still on today's claim files (no `core.db` claims
  yet). Gets: terminals that survive closing the window, attach from anywhere, detach and come back. Test whether a
  launchd-started holder reaches the external disk here.
  Also: `--held` and `--direct` take the conversation's writer lock (section 4 of runtime-design.md) in addition to
  today's claim file, because the host's headless turns still guard only by claim files until M3; `--direct` drops
  the claim file only at M3, once headless turns take the lock. `--direct` holds the lock for its whole life,
  whether or not a core answers. Check: a `--direct` run and a `--held` run on one conversation, the second gets
  busy; an app turn on a conversation a `--direct` run holds is still blocked by the claim file.
- [ ] **M2. Authority into the core** (absorbs A5, one secret store). People, grants, run tokens on the run socket
  (ancestry kept), cards, revoke and suspended rows, the sync lease, doors and their sweep, the resource gate; the
  secrets in the envelope (design 9b): one master key per machine readable only by the signed daemon, XChaCha20-Poly1305 records in core.db; the host vault and kith-auth's per-item writes move into it (the Jev key read once); `auth.json` replaced by a
  tombstone that makes old readers fail closed. Gets: one place that decides. `revoke-access`, `invitee-access`,
  `run-tokens` and `secret-cards` re-run against it.
  Also: the data import (`kith migrate` from `auth.json`, `serve.db` people/grants/vault) with the "same answer" check,
  the delete list (decision parts of `api/roles.rs`, `runauth.rs`, `market/vault.rs`, the people mirror, the host's `setPersonAccess` and `decideAccessRequest`), the
  access-verb rollback rule (`kith rollback`, no per-verb Python rollback; it keeps every revoke and every key, secrets written back as OS-store items; `kith migrate` imports `auth.json` and those items back) and the core-down list, all in
  [runtime-design.md](runtime-design.md) "Execution additions". Freeze the difftest cases as golden first.
  Owner access in M2's scope (Kai, 2026-11-03):
  - Core API methods, in the method table, for the owner to grant and revoke a person's access to an agent, with preset, mode and budget as `kith-auth member` has today. They also cover listing that access, and approving or denying an access request.
  - The generated CLI: `kith people grant|revoke <person> <agent> [--preset … --mode …] [--json]`, plus `kith people requests` and `kith people approve|deny`.
  - The app's People ticks and the access-request card call the same core methods.
  - Owner principal only. The invitee-access and revoke-access checks are re-run against it.
- [ ] **M3. Conversations.** Claims and turns in `core.db`, transcripts held, wake queue and waiters, delegation and
  children, account choice and failover, per-turn claims on warm ACP processes; the host's turn runner and terminal
  claims deleted. Gets: the core holds every conversation, any account resumes. `thread-writers` re-runs.
  Also: import from `live/terminal/*`, and the delete list (`terminal/claim.rs`, `registry.rs`, `agent/bot/turn` and
  `queue`, the claim files, `kith-kernel`'s `threads`). The writer lock stays (the claim files go, the lock files
  never do): warm ACP turns take the writer lock per turn (the holder takes it at `claimed -> running`, the stub
  holds it, released at turn end), built before the claim files are deleted. Check: a turn on a conversation held
  by `--direct` parks as busy and runs after the `--direct` run exits. The writer-lock model's round 4 (holder
  crash ± stub, leaked fd, revoke kill, park and wake, take-over) runs first. Freeze the difftest cases as golden
  first.
- [ ] **M4. Schedules** (was A6). One dialect (cron dialect decision Q1 in core-design.md), routines exported: `kith-sched`
  and the host's routines as one scheduler, `kith schedule ... --json`.
  Also: import of `kith-sched` jobs and the host's routines (idempotent, `--dry-run`, snapshot first); freeze the
  difftest cases as golden first.
- [ ] **M5. The rest of the kernel in Rust** (was A7: packages, agent settings, people and invites, and the Python
  programs the launch still asks). Python removed module by module; per-verb `KITH_CORE=python` rollback kept until
  here. Gets: no Python in the run path; Windows owner use once the hook commands are Rust.

### B. CLI surface (Python for now, ported later)

- [x] **B0. Harness setting.** `kith <agent> set harness H [--model M]`; a Claude model never reaches another harness
  (PR #582).
- [x] **B1. Agent settings.** `kith <agent> set model|effort` beside `set harness`, all with `--json`. The app's agent
  editor calls them. A model per backup entry (issue #578). (PRs #598, #603)
- [x] **B2. Connectors and skills as grants.** `kith <agent> connect <connector>` / `disconnect`, `kith <agent> skill
  add|remove <id>`, list commands with `--json`. The install logic behind the app's Marketplace
  (`serve/host/src/market/`: MCP registry, Anthropic skills, Composio) moves behind these; the Marketplace installs onto
  an agent. Includes the Marketplace layout from 2026-11-03: a **Bots** section for shared kith agents (GitHub topic
  `kith-agent`, needs `kith market --json` and a non-interactive `kith install` preview), and today's "Agents" renamed
  **Engines**.
- B3, B4 (secrets, scheduler): moved to the core as A5 and A6 (now M2 and M4), built once there instead of merged in Python first.
- [x] **B5. Accounts and people.** `kith account add/remove` get app methods; app and bot invites get CLI commands
  (gap map #7, #8).
- [x] **B6. `--json` everywhere a client needs it** (list in the gap map), and the footguns: bare `kith backup`,
  `kith update`, `kith migrate` print usage instead of acting; an unknown word after `kith` doesn't start a run.

### C. The app makes kith agents

- [x] **C1. Built-in `scratch` agent.** Ships with kith; threads with it are the quick chats. "Keep as agent" makes a
  real agent from a thread. Never backed up or shared.
- [x] **C2. "New bot" calls `kith new --json`.** The host keeps only how a bot looks (avatar, pin, order,
  notifications). A "kernel unavailable" state and `kith doctor` checks in the app.
- [x] **C3. Engines and tools in the app go through the CLI** (needs A2, B1, B2) (PRs #635, #636, #637).
- [x] **C4. Memory parity.** Thread and row summaries and shared project memory work on kith memory.
- [ ] **C5. Delete the host-native path.** `hq_agent` required; the host's own bot launch, chat memory, vault use and
  routines for bots removed. Any host bot left on a Mac gets "make it a kith agent" first.

## Order

```
A0 ─► A0b ─► A1 ─► A2 ─► A3 ─► A4 (stopped) ─► M0 ─► M1 ─► M2 ─► M3 ─► M4 ─► M5
B1 ─► B2                  (B5, B6 any time)
C1 any time;  C2 after B1;  C3 after A2+B1+B2;  C4 before C5;  C5 last (after M2, M4)
```

First up: **A0** (core design, then A0b and A1) and **B2** (connector and skill grants in the CLI), in parallel.

## Switching over: one command at a time

- The core is built in `core/` and runs nothing real until a command is switched.
- `kith`'s dispatcher (`kernel/bin/kith`) already sends some commands to the Rust binary (`kith memory check` and the
  `kith serve` subcommands). A command is switched by routing it to the core there.
- **The rule:** a command switches when its same-CLI-line comparison is green across recent merges, so its Python side
  has stopped changing.
- New kernel features keep landing in Python meanwhile, each with CLI-line tests. Those tests join the comparison
  corpus, so the core has to match them too and drift shows up as a failing comparison.
- When nothing is left on the Python side, it is deleted.

## Verification: same CLI line, same result

Every module that moves into the core is checked by running the same CLI lines against the old kernel and the new
core, each in its own throwaway HOME and KITH_HOME, and comparing:

- **What it printed:** stdout, and `--json` compared as data, plus the exit code.
- **What it did:** a diff of the state folders afterwards (files, and rows in the SQLite stores).
- **Normalised first:** timestamps, generated ids, temp paths, ordering where order means nothing.
- **Fakes for the outside world**, as the kernel tests already do: `gh`, git remotes, the Keychain, the engine
  programs (`claude`, `codex`, ...). Nothing real is touched.
- **Intended differences are listed**, each with why (the footgun fixes in B6, the Claude-model guard from B0, ...).
  A difference not on the list fails.
- **The corpus** starts from the existing `kernel/tests/*-check` scripts and every command in the gap map, and grows
  with each bug found.

What it can't see: two things at the same moment (two writers on one thread, a revoke during a run). The TLA+ models
and property tests (`thread-writers`, `revoke-access`, `secret-cards`, `run-tokens`, `invitee-access`) stay the spec
for those, and run against the new core.

## Rules for whoever picks a step up

- CLI command and `--json` first; the app calls it. Nothing new that only the app can do.
- One step, one PR. Tick the box and add a line to the log in the same PR.
- A change that touches people, invites, secrets, threads or tokens runs the matching property checks (see the skills
  `invitee-access`, `revoke-access`, `secret-cards`, `thread-writers`, `run-tokens`).

## The `rust-rebuild` branch

Track A is built on the `rust-rebuild` branch (user, 2026-11-03, overnight mode). Other managers merge into it too.

- Track A PRs branch from `rust-rebuild` and target it.
- `ci.yml` runs only for `main`, so these PRs get no automatic CI. The manager runs the kernel and core tests locally
  before merging, and runs CI by hand (`gh workflow run ci.yml --ref rust-rebuild`) at milestones (after A0b and A1
  land) and fixes what it finds.
- `origin/main` is merged into `rust-rebuild` at least once a day. No iOS changes on it.
- `rust-rebuild` goes to `main` later through one normal PR with full CI.

## Log

- 2026-11-03: plan written; decisions recorded. B0 done in PR #582. Issue #578 opened for a model per backup entry.
- 2026-11-03: fresh core instead of a whole-app rebuild; verification is the same CLI line giving the same result.
  Secrets and scheduler moved from B to the core (A5, A6). A0 (design) and A0b (differential harness) added before A1.
- 2026-11-03: the core is built beside the kernel in a top-level `core/` folder of this repo (not a new repo, not
  under `serve/`), and switched over one command at a time when its comparison stays green.
- 2026-11-03: A0b done: `kernel/tests/difftest/kith-difftest` runs corpus cases on an OLD and a NEW side and reports
  green/red per command (`--summary`); seed corpus passes OLD against OLD. `kernel/bin/kith` does not read KITH_CORE yet.
- 2026-11-03: A0 done: [core-design.md](core-design.md) (crate `kith-kernel` in `core/kernel`, per-command switching through `kernel/lib/core-switched.txt`, `KITH_CORE=python|rust` for the harness), reviewed by the architect agent (12 points, each adopted or answered in its section 13).
- 2026-11-03: A1 done: crate `kith-kernel` (`core/kernel/`, an out-of-tree member of the `serve/host` workspace), module `paths`; the host's `machine_dir`/`state_dir`/`home_dir`, `hqmem::Layout` and the `auth.json` stamp call it. `kernel/bin/kith` routes `kith paths` through `kernel/lib/kith-core.sh`; `paths` is the first line of `kernel/lib/core-switched.txt`, switched on a green harness run (34 `paths-*` corpus cases, all the same; `kith-difftest --rust`). `KITH_AGENTS` is now part of the folder rules in `kith_paths.py` too. CI: `kith-difftest --rust` and `kernel/tests/kith-core-deps-check` in the host-build job.
- 2026-11-03: C1 CLI: built-in `scratch` agent (`kernel/agents/scratch`, no install step, listed as `builtin`, never backed up or shared), engine and model per chat and kept on `--resume`, `kith scratch keep <thread> <name>` makes a real agent via `kith new --instructions` (PR #597). The app half of C1 is still open.
- 2026-11-03: C1 app: Quick chat + Keep as agent (PR #607).
- 2026-11-03: B2 done: kernel CLI in #590, host mirror + `kith connector` in #589, `kith market`/`install --preview` in #593, and the app (this PR): the Marketplace puts connectors and skills onto a kith agent through the CLI, adds the Bots section, renames Agents to Engines; the bot page lists and removes the agent's connectors and skills. Custom hand-written skills are hidden (no CLI path); secrets stay in the host vault until A5.
- 2026-11-03: B1 done: `kith <agent> set model|effort`, --json on every set, a model per backup entry (`claude@sonnet`), the app's agent editor calls them (PRs #598, #603).
- 2026-11-03: B6 footguns done: bare `kith backup|update|migrate` print usage (verbs `backup run`, `update self`, `migrate run`); an unknown word after `kith` never starts a run (PR #596). The rest of B6 (--json list) stays open.
- 2026-11-03: A2a: engines table in core (PR #615): module `engines` in `kith-kernel`, 29 rows (the host's 27 plus hermes and dsh, the kernel's native engines) with aliases, ACP modes, install/login/sign-in probes as data, `oneshot`, env and `enforces`; the host's `backends.rs` reads it (wire shape pinned by a golden test); `kith engines [--json]|install ID|login ID` (`kith kernel engines`) switched in `core-switched.txt`. A2 stays open for A2b (kith-acp, run_once, guest refusal, harness.py per-engine code).
- 2026-11-03: C4 done: the row shows the thread summary (PR #606), summaries moved to `hqmem/summary.rs` with `kith memory summary show|forget|write --thread --json` (PR #608), and `kith memory rm <file> [--json]` + `kith memory forget <text> --json` with `kithMemoryDelete` / `kithMemoryForget` and the apps' Delete file and Forget a fact (this PR). Not carried over: pin, archive, clear, add entry, the shared switch, promotions (no CLI); shared project memory is the kith user layer (user.md, people/, projects/).
- 2026-11-03: C2 done: `kith new --json` (PR #611), host `newKithAgent`/`kernelStatus` (PR #613), the app's New bot makes a kith agent and shows a kernel-unavailable state (this PR). iOS New bot still uses createBot (follow-up after rust-rebuild reaches main).
- 2026-11-03: A2b-1: crate `kith-acp` (`core/acp/`, the one async crate; out-of-tree member of the host workspace): the host's ACP client moved there (`agent/acp.rs` is a re-export plus `init()`), with `run_once` (spawn the engine's ACP command, initialize, session, model, task, collect text, stop reason, usage and reported cost, stop the process group) and `kith kernel run-once ENGINE [--model --cwd --mcp-config --json -- TASK]` (module `acp` in `kith kernel version`; exit 78 when the row has no local ACP mode, so the caller runs the engine its own way). `harness.py` delegates a fresh one-shot to it when `run-once` is switched (`core-switched.txt`) or `KITH_CORE=rust`; the verb is NOT switched yet (real-engine cost parity unverified; the fake-engine difftest `run-once-*` is green, its differences listed in `intended.json`). Fake ACP agent: `kernel/tests/fakes/fake-acp-agent`. `kith-core-deps-check` also bans the server stack from `kith-acp`. The launch half (A2b-2) and the A2 box stay with the launch worker.
- 2026-11-03: B6 done: --json on the gap-map commands: kith-pkg verbs versions/history/pin/unpin/rollback/restore/share/deps/update/push/commit (PR #614); doctor, remote ls|add|rm, ask ls, bug list|show|create, backup add|remove, update self --check, wake status, kith-lifecycle status, kith-chain seen|sig|ls, project and project notes (PR #616); schedule ls|add|rm|pause|resume|run|log (PR #618); auth ls|granted|grant|revoke, no secret values (this PR). Each has a CLI-line check in CI and difftest cases in corpus/json-*.json. Left as is: bare `install --json` still needs --preview/--yes; `update --all --json` and `bug send --json` have no JSON form.
- 2026-11-03: A2b-2: `kith <agent> --launch-json` for every engine of the table with an ACP mode: the launch object gains `engine`, `acp` ({command, args, env}), `registry` and `role_prompt` (from `kith kernel engines launch ID --json`; claude and codex print what they always did, golden-tested); the guest refusal in `kernel/bin/kith` is driven by the table's `serves_guests` (tool hooks + permission modes + per-run MCP: Claude only today); the host runs a kith agent on any ACP engine (`runnable`, `Launch::other_engine`, ACP's own session parameters, instructions as `systemPrompt` or in front of the first message per `role_prompt`) and a quick chat's `-H` takes any ACP engine. Interactive `kith <agent>` in a terminal is unchanged (the 8 kernel harnesses through `harness.py`; extending to the table's other engines is A3). Permission modes of non-Claude/Codex engines are not set by the host yet..
- 2026-11-03: B5 done: account add/remove app methods and Accounts view (PR #622, #626), `kith people ls|show`, `kith invite add|ls|revoke`, `kith bot ...` --json (PR #623), host `people` reads kith-auth and joins host-only facts (PR #628), the desktop People view handles null createdAt, everyoneAgents (read-only) and a load error with Retry (PR #629); the host's access reads follow kith-auth instead of the store mirror (PR #632). Left: the store mirror is still written (fyi d459/d460); plan usage stays host-only (fyi d454).
- 2026-11-03: A3-2 accounts (PR #633): module `accounts` in `kith-kernel` (read side of `kith_accounts.py`: entry ids, main account, readiness, `backups-json`, `launch-env`, `main-env`, `session-run [--sh]`, `session-account`, plus `kith account list`) as `kith kernel accounts VERB` (module `accounts` in `kith kernel version`); difftest `accounts-*` (25 cases, `program` field runs the Python helper on OLD) green. Not switched: the launch assembly calls the verbs; writes (`account add|use|remove`, `backup add|remove|set|reset`) and `account use`/`backup list` JSON stay Python. A3 stays open.
- 2026-11-03: A3-3 secrets read half (PR #643): module `secrets` in `kith-kernel` (`load_auth`, `cut_off`, `may`, `resolve`, `resolve_bot`, jobs/doors reading, and `take_once` under the same `auth.json.lock` flock and atomic write as `kith-auth`) as `kith kernel secrets resolve|may` (not listed in `kith kernel version`; nothing routed); difftest `secrets-*` (33 cases: owner, members, revoked, bots, helpers with `--via`, once-grant consumed, held, old and damaged files, jobs) green with no intended differences, plus 67k generated `resolve` calls from `kith-auth-props` equal to Python. A3 stays open.
- 2026-11-03: A3-1 prompt assembly (PR #634): `launch::prompt` and `launch::facets` in `kith-kernel` (agent.md body and protocol with placeholders filled, long-term memory through the facets load filter, start files, `<session/>` tag, guest `<limits>`); `kith kernel launch prompt AGENT [--person --project --pkg --json]`, not routed or switched. 14 difftest cases (`--launch-json` `system_prompt` vs the verb) are the same. Gap, narrower only: a Tidy-promoted note keeps its folder audience. A3 stays open.
- 2026-11-03: A3-4 settings + MCP (PR #639): `launch::settings`, `launch::mcp`, `launch::guard` and `launch::machine` in `kith-kernel` (permission mode, guest deny rules, settings fill, hooks, guest sandbox, MCP merge); `kith kernel launch settings|mcp AGENT --json`, not routed or switched. 67 difftest cases (`--launch-json` `settings`/`mcp_servers` vs the verbs) are the same, two intended: a home folder with `&` or a backslash (bash's sed mangles it, Rust writes it literally). A3 stays open.
- 2026-11-03: C3 done: a kith agent's engine, model and effort in the desktop and iOS editors call `kithSetSetting` (`kith <agent> set harness|model|effort --json`) and show the CLI's error; host `updateBot` refuses a change to backend/model/effort/fast/connectors/skills on a kith agent with the kith command to use (unchanged echoes pass); the host-native engine picker, connector/skill toggles and `/fast` are hidden for kith agents (kith has no Fast mode); a connection setup card runs `kith <agent> connect`; Marketplace `enable_new` skips kith agents; skill-read errors are shown (PR #635). Engine install and sign-in run the line `kith engines install|login ID` prints, the quick-chat engine list comes from `kith engines --json` (`kithEngines`), backup entries take `@model` (PR #636). iOS: `kithSetSetting`, harness/model/effort rows, kith-owned fields never sent by save (PR #637). Left host-native: sign-in state probes (`kith engines --json` has `signed_in: null`), vault env and Composio/custom MCP/credentials (A5), the per-thread engine kv, iOS New bot. Not run in a live app; desktop typecheck + unit tests, host tests + clippy 1.99, iOS Kit tests + iOS Dev build.
- 2026-11-03: A3-5 whole launch (this PR): `launch::assemble` in `kith-kernel` runs every caller-side step of the launch branch of `kernel/bin/kith` (flags, `secrets` resolve with `--via`/`--take-once` and the held wait, the membership budget, the monthly limit, snapshot pinning and the `--hold` marker, project lookup, prompt, accounts, engine gate, settings, `kith-permit apply`, MCP, the object with Python-formatted `claude_args`) as `kith kernel launch AGENT --launch-json [flags] [--kernel DIR]`. Programs still asked: `kith-pkg resolve`, `kith-cost limit|spent`, `project-id`, `kith-auth mcp-config` (A5), `kith-permit apply`, `claude`, `xcode-select`. Not switched: 90 difftest cases (`launch-whole-*`: owner, codex, qwen over ACP, -H/-m/-e, --backup entries, member/bot/revoked guests, helper `--via` with a once-grant consumed, scratch, limits, resume refusals) are the same with 6 intended differences (listed in `intended.json`), but the switch waits for the Linux fix of the guest-sandbox probe in `secrets` and green CI. A3 stays open.
- 2026-11-03: A3 switched: `launch` is listed in `kernel/lib/core-switched.txt` (module `launch` in `kith kernel version`), and `kith AGENT --launch-json [flags]` goes to `kith kernel launch AGENT --launch-json ... --kernel DIR` before anything is asked of kith-auth (so a once-grant is taken by one side only). The folders come from what the caller gave, not from the router's own export, and the program is found as for `paths` and `engines`. No program, an older one without `launch`, `KITH_CORE=python` or removing the line: the script runs, unchanged. Interactive `kith AGENT`, `--print`, `--resume` and an unknown agent stay in the script. New `kernel/tests/kith-launch-once-race-check` (two launches at once on one helper once-grant: exactly one takes it, auth.json stays valid, both paths; run in CI after the difftest). Full difftest 371/371 with the switch on (after merging #650; 302/302 before) and kith-difftest-check 34/34. The launch still asks `kith-pkg resolve`, `kith-cost`, `project-id` and `kith-auth mcp-config` (A5) as programs, so the 50 ms target is not met yet: measured on a debug build, `--launch-json` takes ~275 ms through the Rust verb against ~810 ms in Python (a bare agent, on a loaded Mac); what is left is those Python programs' start-up, and the target waits until they move (`kith-pkg`, `kith-cost`, `project-id`: A7; `kith-auth mcp-config`: A5). New `kernel/tests/kith-auth-mixed-race-check` (10 rounds of 4 Rust launches taking one once-grant while 8 Python `kith-auth` writes run on the same auth.json: valid JSON, no write lost, the grant used exactly once; it fails 9 of 10 rounds with the lock removed from `secrets`), run in CI next to the once race check.
- 2026-11-03: A4-1 delegation limits (this PR): module `delegation` in `kith-kernel` holds the hop caps (8 on one Mac, 2 across), the REFUSED exit 4, the chain cost cap (default US$5, `delegation.chain_cost_cap_usd`), how a record is read (old registrations read as a root ask at hop 0, cost unknown; damaged ones are skipped), the refusal decision with its stderr words, and `parked`; `kith kernel delegation limits|cap|record ID|check [--id ID] [--crossing]|parked ID` (read/decide only, the check takes `live/ask/.lock` as `open_record` does; a bare record read takes none, as Python). The host's `HOP_MAX`, `CROSS_MAX`, `MAX_REQUEST_HOPS` and `parked_in` now come from the crate (data only; the host still writes nothing new). 69 difftest cases (`delegation-*`; OLD runs `kernel/tests/difftest/delegation-old.py`, which calls kith-ask's own `open_record`, `read_reg` and `cap_cmd`) are the same with no intended differences: root, hop 6 to 10, cross hops 1 to 4, cost under/at/over the cap, cap unset/number/none/broken file, old, damaged and missing records, a chain handed over in `KITH_ASK_*` with bad numbers. Not switched: `kith-ask` still decides and writes; no line in `core-switched.txt`. Left for A4: the thread writer claim (`threads`). A4 stays open.
- 2026-11-03: A4-2 threads (this PR): module `threads` in `kith-kernel` ports the claim half of `kernel/bin/kith-sessions` (`resolve`, `claim`, `terminal`, `proc`, `sweep`, `record`, the registry reading `resolve` needs) and the file protocol of `serve/host/src/terminal/{claim,registry,intent}.rs` (`live/terminal/<sid>.{json,claim,takeover.claim}` and their flock lock files, `sessions/intent|decisions`), byte-compatible with the Python writer, as `kith kernel threads VERB` (module `threads` in `kith kernel version`; plus `release PID` and `holder SID` for tests). The only `unsafe` in the crate is `threads/os.rs` (kill(2), `libc`; the crate lint went from forbid to deny). 55 difftest cases (`threads-*`; OLD runs `kernel/tests/difftest/threads-old.py`, kith-sessions itself): free thread, claimed by a live oneshot/app/bot/unknown-kind writer, stale claim (dead pid, reused pid), old pid-only file, damaged files, two sessions, take-over in progress, intent and decision ended by a taker, prune of stale neighbours, proc claim, resolve (free, held, stale, take-over, unknown thread, folder gone, registry agent/cwd, other login, CLAUDE_CONFIG_DIR) and record (terminal, one-shots, not listed, worktree, compact) are the same, two intended (kith-sessions dies with a traceback on a claim whose pid is no number; it resolves the thread `*` by glob). `kernel/tests/kith-threads-race` runs `kith-sessions` and the Rust verb against one thread at once (fresh, dead, reused-pid, damaged and live claims, three rounds with hand-over): never more than one winner; with the lock taken out of the Rust side it fails within ten iterations, so it can see the bug; CI runs it. thread-writers (TLC) passes: every row as expected. The differences between `kith-sessions`, `claim.rs` and the crate are in the PR text. Not switched: no line in `core-switched.txt`, `kernel/bin/kith` untouched, the host still uses `claim.rs`. A4 stays open: the host's own paths (`stop_holder`, the revoke `sweep` with `keep` and its intents, `holder`'s App rule, `registry::read_all`/`liveness`) and the wrapper (`kith-sessions run|pick|latest`) are not in the crate, so there are still two implementations until the host moves onto it with the verbs.
- 2026-11-03: runtime design agreed (Kai); copied to `docs/plans/runtime-design.md`. A4 stopped (A4-1 and A4-2 stay as
  written; `threads` not switched, host not moved onto claim verbs: claims go into `core.db` in M3). A5, A6, A7 replaced
  by M0-M5 (M2 absorbs A5, M4 is A6, M5 is A7); `core-design.md` marked superseded from A4 onward. Docs only.
- 2026-11-03: M0a (PR for `flow/rust-folder-rules-m0-core`): crate `kith-core` (`core/runtime/`, an out-of-tree member of the `serve/host` workspace; depends on `kith-kernel`, never the reverse). `core.db` (SQLite, WAL, `BEGIN IMMEDIATE`, migrations table, generation and claim-id counters) lives in `<machine>/core/` with `core.lock` (flock / LockFileEx; whoever takes it bumps the generation, embedded included), the marker (refuses an empty start when `core.db` is missing or another), owner-only modes (Windows: `icacls` check). One door, `Core::claim_tx`, refuses a stale generation (unused until M3). `quick_check` at daemon start only; migrations only via `start`/`migrate`, after `integrity_check` and a `VACUUM INTO` snapshot; embedded creates a fresh `core.db` but never migrates. Embedded mode runs the switched verbs (`kith paths`, `kith kernel ...`) through the core, bare when a daemon holds the lock or no core.db exists yet (a read-only verb never creates the core, so the difftest, which compares files, stays at 100% - 426/426 cases; `serve/host/tests/core_embedded_e2e.rs` proves the real binary takes the lock and bumps the generation). `core/` joins the protected-paths and guest-sandbox lists (9b rules 1 and 2). CI: `kernel/tests/kith-core-boundary-check` (+ `--selftest`) and a `windows` job; `kith-kernel`'s `launch`, `secrets` and `threads` modules are `cfg(unix)`. No method table (M0b), sockets or tokens (M0-4), holder (M0-3).
- 2026-11-03: execution additions and the architect's M0 answers written into `runtime-design.md` (data import per
  milestone, oracle after Python, delete lists, branch sync; crates `kith-core`/`kith-api`/`kith-holder`, access-verb
  rollback, core-down list); M0a-M0f steps with checks, and the per-M additions, listed here. Kai: `rust-rebuild` goes
  to main after M0c is fine; managers never merge into main. Docs only.
- 2026-11-03: architect's six review fixes to #658 applied to `runtime-design.md` and here: snapshot-restore wording, `kith rollback` also writes secrets back (property: keeps every revoke and every key), M2 envelope wording, "M0 build rules" block (M0a, M0d, M0f) and the line-77 `quick_check` edit, M0c no-holder rule, trace-checking notes (`Runtime.tla` as a verify skill, events from M3, one committed transaction per event). Docs only.
- 2026-11-03: architect's two fixes to #659 applied: `runtime-design.md` M0a now says embedded calls skip `quick_check` (hooks call embedded mode, so each call stays cheap), and the access-verb rollback in `runtime-design.md` and here says `kith migrate` imports `auth.json` and those OS-store items back. Docs only.
- 2026-11-03: M0d `kith-holder` (this PR, base `rust-rebuild`): crate `core/holder` (out-of-tree member of the host workspace; deps serde, serde_json, libc / windows-sys; `scripts/check-holder-deps.sh` in the `host` job fails if it links any other kith crate). Adapted from herdr-core b63ecbd (NOTICE, headers kept): the frame layout, the platform layer (Unix PTY, ConPTY, sockets and overlapped named pipes with owner-only access, detaching, Windows command lines), the pump/reap/tail/backlog shape of the holder. Kith's own: hello with version + opaque secret (the local hand's pairing key, M0f) + core generation; spawn(conv, claim, gen, argv, env, cwd, pty|pipes) accepted only for the newest generation and the secret (same conv+claim answers the running one); input, resize, tail, list; kill idempotent over the whole tree (Unix: own session + killpg, "gone" only once the group is empty; Windows: one job object per spawn), unknown id answers "gone"; exits kept until acked. `kith-holder start` launches one detached (own session; CREATE_BREAKAWAY_FROM_JOB) and exits. 7 integration tests on a real holder process (spawn/list/kill/ack, terminal input and tail, wrong secret, stale generation and takeover, exit kept across cores, tree kill, holder and process outlive the process that started it) plus `windows` job in `ci.yml` (also on workflow_dispatch; adds kith-core when `core/runtime` is on the branch). Not done: ConPTY and named pipes only compile-checked here (CI runs them); no real launchd or Windows supervisor run (`core/holder/tests/manual/survive-supervisor.sh` is by hand); daemon wiring is M0c, networking/lease M0f, the method table M0b.
- 2026-11-03: owner access in M2 (Kai): the owner grant and revoke of a person's access to an agent, its listing, and approve or deny of access requests are core API methods with a CLI (`kith people grant|revoke|requests|approve|deny`) and the app calls the same methods; owner principal only, invitee-access and revoke-access re-run. The host's `setPersonAccess` and `decideAccessRequest` go on M2's delete list (both docs). Docs only.
- 2026-11-03: A3 accounts port (this PR, `flow/rust-folder-rules-3-accounts-port`): `kith-kernel`'s `accounts` matches main #599 and #577 again. `accounts/{settings,state,pick,listing,mirror,clock,pyjson}.rs`: Python-compatible JSON, the managed set (policy, pins, per-agent sets, cross-harness layout, settings migration), `state.json` (flock on `state.json.lock`, atomic replace, corrupt file kept as `.corrupt`, legacy marks imported), `pick`, `list --agent`, the Claude mirror (settings, plugins, auto-mode flags), `launch-env --record`, `main-env --state|--record|--prefer`. Launch records `last_started`, takes `--account`, and follows main's launch changes (readfence hook, guest minimum Claude 2.1.295, guest domains). The 96 "not yet ported" intended entries are gone and `launch` is back in `core-switched.txt`. The difftest masks integers that read as a time (1.5e9 to 2e9 seconds, or the same in ms) in JSON data only, never text or other numbers, because `last_started` is a clock reading; 20 new corpus cases cover pins, policy, state (missing, empty, corrupt, pre-#599), pick, `--record` and `--account`. Account writes (`out`, `clear`, `usage`, add, remove) stay Python.
- 2026-11-03: F3-F6 of the architect's final review (this PR, `flow/rust-folder-rules-4-final-fixes`, base `rust-rebuild`): F3 the Rust guest-sandbox probe asks the OS only (`secrets::os_home`, absolute programs, no `PATH`, no `$HOME`; no answer is a guest); F4 `KITH_BIN` is taken only if executable and `memory hook --help` says "phase", else the running program, and `kith_core_exec` execs with `KITH_BIN` set to the program it checked; F5 built-in agents (the kernel's own, e.g. scratch) are the owner's alone: `kith-auth member` refuses them, an old own or `*` row, a bot row or a helper list gives nothing, in Python `may` and in the Rust `secrets` (launch included; the host's people mirror is F8, not touched); F6 the router deny sweeps cover `kith_tools.rs` and `scratch.rs`, the launch-once race check races Python against Rust on one grant, the mixed race check has Python `resolve --take-once` racers, both assert the Rust path ran (a wrapper that notes each `kernel launch`); invitee-access, secret-cards and revoke-access re-run and synced. Difftest cases for F3, F4 (two intended differences: Python searches for another kith, Rust uses itself) and F5.
- 2026-11-03: #680 review round: the F4 difftest cases set `CARGO_TARGET_DIR` away so Python finds no kith on any machine (the intended difference now holds on a Mac and on CI); revoke-access `SYNC` stays at `fb5f140f` (the ~940 changed lines are unread; all 80 rows pass at `0872ea58`); the mixed race check has a sequential Rust-then-Python case and the reverse; `secrets::os_home` reads the password database with `getpwuid_r` (no `id`, no timeout), a test shows the runner's own user is no guest; a `KITH_BIN` that is the running program skips the `memory hook` probe; built-ins need a plain name (no `/`, no leading dot) in both Python and Rust.
- 2026-11-03: M0b `kith-api` (this PR, base `rust-rebuild`): crate `core/api` (out-of-tree member of the host workspace; depends on `serde_json` only; `kernel/tests/kith-core-boundary-check` fails if it lists or names another crate of this repo, and `kith-core-deps-check` covers its tree; `kith-holder` still links no kith crate). The table (`src/table.rs`) has one row per method: name, params (type, required, how written on the command line), result, allowed principals (owner, member, bot, run-token; every row is owner-only until M2), CLI shape (path, aliases, `--json`-style formats, required switches), stability, who serves it today. Params and results are written in the contract's own type language (`string`, `bool`, `int`, `json`, `Name`, `[T]`, `{T}`, `T?`), so `contract.json` seeds it as it is (`src/types.rs` is the contract's enums and types as data; a test fails if the two differ). Rows: the switched verbs (`paths`, `kernel version|engines|launch|accounts|delegation|secrets|threads|run-once|core-migrate`; Served::Core), the five verbs the host reads from the kernel (`ls`, `project ls`, `AGENT --launch-json`, `new`, `ask delegations`; typed from the contract) and the B6 `--json` forms (auth, doctor, remote, ask, bug, backup, update self, wake, project, the kith-pkg verbs, install, schedule; experimental, result `json` until their types follow). `cli::parse_argv` turns a command line into a row and its params (longest path wins; no row means NotInTable and the caller falls back as today); tests hold it to every `contract.json` verb, every switched verb and every `--json` line of difftest `corpus/json-*.json`. The real CLI is NOT switched to it (M0c wires the daemon and the CLI to the parser). `core/api/openrpc.json` is generated (`cargo run --manifest-path serve/host/Cargo.toml -p kith-api --bin kith-api-gen -- --write`; `--cut` writes `released.json` and refuses a table that breaks the last one); a test fails when the file differs from a fresh generation (\r\n normalised, `.gitattributes` pins LF). `core/api/released.json` is the first cut; `compat::breaks` fails on a removed stable method, a removed or renamed param or field, an optional param made required (or a new required one), a narrowed type (scalar, enum value, list item, field turned required, nullable made plain), additions pass; experimental rows in the snapshot may change. `kith_core::call(paths, principal, method, params)` checks the row, the principal and the params, answers `paths` and `kernel.version` itself and answers `NotServed` for every other row (the seam M0c fills with the host's handlers; `stateless_through_core` and `through_core` stay until then). CI: `kith-api` tests in the `host` and `windows` jobs. Not done: serde request/result structs, TypeScript and Swift types (types follow-up), B6 result types, kith-lifecycle/kith-chain tools (not `kith` verbs), a handler registry.
- 2026-11-03: M0c daemon front (this PR, base `rust-rebuild`): `kith-core` gains `Core::call(&self, ...)` (replaces the free `call`), the owner socket (reuses the holder crate's owner-only Unix socket / named pipe layer; `<machine>/core/owner.sock` 0600 in a 0700 folder, `owner.token` 0600; hello with protocol + method-table hash, then the token; JSON-RPC lines), the daemon (takes `core.lock` and bumps the generation, `quick_check` at start; waits out an embedded holder of the lock; refuses a second daemon; never blocks `kith serve`) and the holder link (starts `kith-holder` detached, hello with the generation, records the instance in `core.db`, kills the previous instance's process groups by pid + start time on a change; a `refused` frame is only a failed connection; an `instance` unknown in an M0a database kills nothing). Method table: `Arg::Caller`, `paths` takes the caller's home/env (daemon-mode verbs use the CLI's environment), rows `kernel.coreStatus` / `kernel.coreDaemon`, `schema_hash()`, the `x-kith-cli` break check for stable rows. Host: `core_front.rs` (the CLI asks the daemon for `paths` only (never `kernel version`: the bash router asks that of one binary and execs it, a daemon of another build must not answer), sending only the variables `paths` reads, else runs as before; a version/hash mismatch falls back with one line in core.log (`front:`) and nothing on stderr; a usage error on a row Python still serves falls back), verbs `kernel core-daemon` and `kernel core-status`, the daemon spawned inside `serve()`; `kith doctor` has a "kith core" section (daemon pid + generation, socket, holder, core-down status naming the recovery verb). Difftest `--daemon` pass (NEW side runs a real daemon in its sandbox; corpus `paths-daemon.json`; `core/` and `core.log` are intended differences): 456/456 the same in `--rust`, `--rust --core` and `--rust --daemon` (312 cases through a daemon, 70 calls answered; core.log lines starting `daemon:` / `holder:` and a log folder only the daemon made are masked, nothing else); `doctor-json` now sets `bin_both` so both sides report the same "kith core" section. Tests: `serve/host/tests/core_daemon_e2e.rs` (real daemon, CLI and holder processes: socket modes, restart bumps generation and waits for a running command, no holder / holder killed mid-run stop neither the daemon nor the CLI), 35 kith-core tests. Decision: the release does not bundle `kith-holder` in M0c, the daemon runs without it (M1 bundles it). Guests: `<machine>/core` was already on the guest deny list (9b rules 1-2, `kith-guest-sandbox-check`). Not done: Windows named-pipe run (kith-core cannot build for Windows on this Mac: bundled SQLite needs a Windows C toolchain; kith-api and kith-holder were clippy-checked for `x86_64-pc-windows-msvc`; the CI `windows` job decides), the writer lock (core-down item 3, `--direct`), `host.sock` (M0e), claims (M3).
- 2026-11-03: F0 of the architect's final review (writer lock, TLC round 3 green): section 4's writer-lock bullet and its refinements copied into `runtime-design.md`, the `--direct` fallback takes the lock instead of a file claim, core-down item 3 is the `--direct` writer lock, Verified round 3 added; the build order is in M0d (the holder takes it per engine; #669 or a follow-up), M1 (`--held`, `--direct`) and M3 (claim files go, the lock stays; model round 4 first). Docs only.
- 2026-11-04: M0e run socket and token table (this PR, base `rust-rebuild`): `kith-core` gains `<machine>/core/run.sock` (Unix socket 0600 in the 0700 core folder, Windows named pipe `kith-run-<hash>`; the holder crate's owner-only layer, plus `platform::peer_pid` there: `SO_PEERCRED` / `LOCAL_PEERPID` / `GetNamedPipeClientProcessId`), served by the daemon next to the owner socket (a daemon that cannot open it logs `daemon: no run socket` and still serves the owner) and usable without a daemon in tests. Token table `runs.rs` (memory only; SHA-256 of the token, never the token): `run.mint` (agent, person, conversation, claim, room, project) / `run.bind` (pid; the process start time is kept as in `core.holder`) / `run.end` are owner-only table rows, answered by the daemon; a token resolves only when it is known, bound, its peer's pid descends from the bound pid (`descends` kept from `runauth.rs`, `ps` walk) and the bound process is still the one with that start time (an exited run or a reused pid kills the token). Wire form is host.sock's (`{token, method: recall|memorySearch|projectId, args}`, one line each way), the args allow-lists (`recall_args`, `memory_args`, `project_args`) are ported with the same texts; the call goes through the table as principal `RunToken` (new rows `run.recall`, `run.memorySearch`, `run.projectId`, the only RunToken rows; `Core::call_run`): `run.projectId` with no args is answered from the token, the two kernel tools answer "isn't answered by the core yet" until a later step runs them. The host-only methods (cards, access, secret, history) are not on this socket. Tests: real-socket tests with real child processes (child, grandchild, two descendants, sibling tree, unbound, ended, unknown/mangled, method outside the subset, end during an in-flight call, exited process, reused pid, owner-only mode, through a real daemon with owner-socket mint/bind/end) and `runs_props.rs`, the port of the `runauth_props` properties (proptest `=1.12.0`); the host's own 29 `runauth` tests incl. its P1-P7 re-run green, `host.sock` and `runauth.rs` untouched. Guests: `core/` stays on the guest deny list as a whole, so `run.sock` is NOT reachable from a guest sandbox yet (allowing it would change today's live guest sandbox; the step that switches host.sock allows exactly this path). A daemon restart ends every token (memory); runs are minted again. Windows: clippy-clean for kith-holder on `x86_64-pc-windows-msvc`; kith-core cannot build for Windows on this Mac (bundled SQLite), the CI `windows` job decides; ancestry on Windows is the run's own pid only until a process-tree walk lands (a grandchild is refused, never let through). Not done: running recall / memory search in the core, switching host.sock, claims (M3).
- 2026-11-03: review cleanup PR (F7, F8, F9 of the architect's final review, plus the collected follow-ups; `flow/rust-folder-rules-4-cleanup`, base `rust-rebuild`, before M1): F7 `terminal/claim.rs` reads an unknown claim `kind` as a live holder judged by pid + start (`Kind::Other`) and `take_as` never removes it; `core/kernel/src/threads*` joins thread-writers `drift.sh`. F8 the 10 s people mirror takes `AUTH_WRITES` around read and write (as does the `people` list) and mirrors `*` rows too (kith-auth never lists a built-in agent), so a `*` agent's events arrive. F9 `run_once` keeps the answer, usage and cost when `Closed` follows the prompt response and reports the cost seen on a crash (`run_once_spent`, `cost_usd` in the error JSON); `random_hex` fails; `delegation::check_and_write` holds the box lock across check and write and a lock that fails refuses; `summary_cli` also asks `guest_sandbox()`; `kith-scratch` needs the host's record that the thread is scratch's when the ledger has no row. Follow-ups: the holder's "gone" sticks once seen; plain doctor words for a holder tied to its starter; `accounts::state::update` and the settings migration note a lock or write failure through `kith_kernel::log` (kith-core points it at `core.log`); `kith-auth agent-access` at launch times out after 20 s (no extra domains, warning); the backup login's `.claude.json` mirror is written under `.claude.json.lock`; the readfence hook runs python3 by absolute path; the core holder's start time is read natively on macOS (the Windows ACL check still uses `icacls`, TODO in `perms.rs`); the F4 difftest cases get a hermetic `PATH`. Skills: thread-writers re-run (SYNC not moved), invitee-access and revoke-access notes.
