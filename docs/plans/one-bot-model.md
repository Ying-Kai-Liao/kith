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
- **The kernel moves to Rust, module by module**, as a crate the CLI and the host both link. The CLI's commands and
  their `--json` stay the contract, so the apps never notice a module moving. Python keeps the parts that are still
  changing fast (packages, agent settings, people) until they settle.
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
| Host memory has features the app shows (thread and row summaries, shared project memory) | Kith memory reaches parity before the host path is deleted (C4) |
| Settings edited in the app become package commits, so they apply on every Mac | Per-Mac choices go in kith settings, like permission mode already does (B1) |

## Tracks

Three tracks. A and B can run in parallel; C needs pieces of both. Each step is one PR (or a few), with a CLI command
and `--json` first and the app calling it after.

### A. Rust kernel crate

- [ ] **A1. Folder rules.** One Rust implementation of where kith's folders are (`kernel/lib/kith_paths.py`,
  `serve/host/src/kernel.rs::machine_dir`, `hqmem::Layout::from_env` disagree today: `.migrated` is respected by two
  and not the third). New crate under `serve/` (name it in the PR), linked by the host, exposed to Python through the
  `kith` binary the way `kith memory check` already is. `kith_paths.py` asks it, or is checked against it in CI.
  Done when: one implementation, the three callers agree, a test proves it. Gap map #6; architect ranked it first.
- [ ] **A2. Engines and ACP.** Move engine detection and ACP launch from `agent/backends.rs` into the crate. The kernel's
  harness list becomes the crate's list (today 8 in `adapters/harness.py` vs 27 in the host). `--launch-json` works for
  every engine, not just claude and codex (`kernel/bin/kith`, the `launch` branch). Done when: a kith agent with
  `harness: gemini` (or any listed engine) runs in the app and the terminal.
- [ ] **A3. The launch path.** `kith <agent> --launch-json` (prompt assembly, settings, MCP config, accounts) in Rust.
  Target: under 50 ms.
- [ ] **A4. Writer claims and delegation limits.** One implementation of the thread writer claim (`kith-sessions` vs
  `serve/host/src/terminal/claim.rs`) and the hop caps (3 copies: `kernel/bin/kith-ask`, `api/delegations.rs`,
  `chat/team/requests.rs`). Model-checked properties exist (`thread-writers` skill); keep them passing.
- [ ] **A5. Onward, as modules settle:** secrets API (after B2), scheduler (after B3), packages, settings.

### B. CLI surface (Python for now, ported later)

- [x] **B0. Harness setting.** `kith <agent> set harness H [--model M]`; a Claude model never reaches another harness
  (PR #582).
- [ ] **B1. Agent settings.** `kith <agent> set model|effort` beside `set harness`, all with `--json`. The app's agent
  editor calls them. A model per backup entry (issue #578).
- [ ] **B2. Connectors and skills as grants.** `kith <agent> connect <connector>` / `disconnect`, `kith <agent> skill
  add|remove <id>`, list commands with `--json`. The install logic behind the app's Marketplace
  (`serve/host/src/market/`: MCP registry, Anthropic skills, Composio) moves behind these; the Marketplace installs onto
  an agent. Includes the Marketplace layout from 2026-11-03: a **Bots** section for shared kith agents (GitHub topic
  `kith-agent`, needs `kith market --json` and a non-interactive `kith install` preview), and today's "Agents" renamed
  **Engines**.
- [ ] **B3. One secret store.** Connector logins, Composio, 1Password, the voice and router keys move from the host
  vault into `kith-auth`. The Jev key stops being read from both (`chat/router.rs` vs `rich_card/jev.rs`). Gap map #2.
  Run the `secret-cards`, `revoke-access` and `invitee-access` checks.
- [ ] **B4. One scheduler.** Host routines become `kith-sched` jobs (`jobs.json` stays the one definition; the host may
  be what runs them). `kith schedule ... --json`. Decide the cron dialect (kith-sched: local time, `@daily`; routines:
  IANA timezone, intervals, once, webhook and event triggers).
- [ ] **B5. Accounts and people.** `kith account add/remove` get app methods; app and bot invites get CLI commands
  (gap map #7, #8).
- [ ] **B6. `--json` everywhere a client needs it** (list in the gap map), and the footguns: bare `kith backup`,
  `kith update`, `kith migrate` print usage instead of acting; an unknown word after `kith` doesn't start a run.

### C. The app makes kith agents

- [ ] **C1. Built-in `scratch` agent.** Ships with kith; threads with it are the quick chats. "Keep as agent" makes a
  real agent from a thread. Never backed up or shared.
- [ ] **C2. "New bot" calls `kith new --json`.** The host keeps only how a bot looks (avatar, pin, order,
  notifications). A "kernel unavailable" state and `kith doctor` checks in the app.
- [ ] **C3. Engines and tools in the app go through the CLI** (needs A2, B1, B2).
- [ ] **C4. Memory parity.** Thread and row summaries and shared project memory work on kith memory.
- [ ] **C5. Delete the host-native path.** `hq_agent` required; the host's own bot launch, chat memory, vault use and
  routines for bots removed. Any host bot left on a Mac gets "make it a kith agent" first.

## Order

```
A1 ─► A2 ─► A3 ─► A4 ─► A5
B1 ─► B2 ─► B3 ─► B4      (B5, B6 any time)
C1 any time;  C2 after B1;  C3 after A2+B1+B2;  C4 before C5;  C5 last
```

First up: **A1** (folder rules in Rust) and **B2** (connector and skill grants in the CLI), in parallel.

## Rules for whoever picks a step up

- CLI command and `--json` first; the app calls it. Nothing new that only the app can do.
- One step, one PR. Tick the box and add a line to the log in the same PR.
- A change that touches people, invites, secrets, threads or tokens runs the matching property checks (see the skills
  `invitee-access`, `revoke-access`, `secret-cards`, `thread-writers`, `run-tokens`).

## Log

- 2026-11-03: plan written; decisions recorded. B0 done in PR #582. Issue #578 opened for a model per backup entry.
