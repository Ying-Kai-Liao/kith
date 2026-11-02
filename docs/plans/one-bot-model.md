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
- [ ] **A3. The launch path.** `kith <agent> --launch-json` (prompt assembly, settings, MCP config, accounts) in Rust.
  Target: under 50 ms.
- [ ] **A4. Writer claims and delegation limits.** One implementation of the thread writer claim (`kith-sessions` vs
  `serve/host/src/terminal/claim.rs`) and the hop caps (3 copies: `kernel/bin/kith-ask`, `api/delegations.rs`,
  `chat/team/requests.rs`). Model-checked properties exist (`thread-writers` skill); keep them passing.
- [ ] **A5. One secret store** in the core: kith-auth's model plus everything in the host vault (connector logins,
  Composio, 1Password, voice and router keys); the Jev key stops being read from both (`chat/router.rs` vs
  `rich_card/jev.rs`). Gap map #2. The `secret-cards`, `revoke-access` and `invitee-access` checks must keep passing.
- [ ] **A6. One scheduler** in the core: `kith-sched`'s jobs and the host's routines as one thing, `kith schedule
  ... --json`. Decide the cron dialect (kith-sched: local time, `@daily`; routines: IANA timezone, intervals, once,
  webhook and event triggers).
- [ ] **A7. Onward, as they settle:** packages, agent settings, people and invites.

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
- B3, B4 (secrets, scheduler): moved to the core as A5 and A6, built once there instead of merged in Python first.
- [x] **B5. Accounts and people.** `kith account add/remove` get app methods; app and bot invites get CLI commands
  (gap map #7, #8).
- [x] **B6. `--json` everywhere a client needs it** (list in the gap map), and the footguns: bare `kith backup`,
  `kith update`, `kith migrate` print usage instead of acting; an unknown word after `kith` doesn't start a run.

### C. The app makes kith agents

- [x] **C1. Built-in `scratch` agent.** Ships with kith; threads with it are the quick chats. "Keep as agent" makes a
  real agent from a thread. Never backed up or shared.
- [x] **C2. "New bot" calls `kith new --json`.** The host keeps only how a bot looks (avatar, pin, order,
  notifications). A "kernel unavailable" state and `kith doctor` checks in the app.
- [ ] **C3. Engines and tools in the app go through the CLI** (needs A2, B1, B2).
- [x] **C4. Memory parity.** Thread and row summaries and shared project memory work on kith memory.
- [ ] **C5. Delete the host-native path.** `hq_agent` required; the host's own bot launch, chat memory, vault use and
  routines for bots removed. Any host bot left on a Mac gets "make it a kith agent" first.

## Order

```
A0 ─► A0b ─► A1 ─► A2 ─► A3 ─► A4 ─► A5 ─► A6 ─► A7
B1 ─► B2                  (B5, B6 any time)
C1 any time;  C2 after B1;  C3 after A2+B1+B2;  C4 before C5;  C5 last (after A5, A6)
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
