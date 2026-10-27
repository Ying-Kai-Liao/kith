---
name: run-tokens
description: Property tests that a guest run (an invited person's or a bot guest's agent run) reads nothing beyond its run token's audience. Use when a change touches serve/host/src/runauth.rs (TokenTable, resolve, RunGrant, authorize, recall_args, memory_args, project_args, handle_request, the descends ancestry check), the host.sock listener (serve_one, bind, start), where runs get tokens (grant_for, grant_for_env, the callers in agent/bot/hq.rs and channels/run.rs), kernel/bin/kith-recall (ask_host, the guest branch), kernel/lib/kith_runsocket.py and the guest branches of kernel/bin/kith-memory (search) and kernel/bin/project-id, or the env scrub of KITH_RUN_TOKEN / KITH_RUN_SOCKET.
---

# A guest run reads nothing beyond its token's audience

An invited person's or bot guest's run gets a host-issued token, bound to person + agent + room + run id and to the run's pid, held in memory and revoked when the run exits. `kith recall` in such a run (and `kith-memory search`, `project-id`, with methods `memorySearch` and `projectId`) sends `{token, method: "recall", args}` over `$KITH_MACHINE/run/host.sock`; the host takes who is asking from the token alone. Unit tests check hand-picked requests; these check generated ones, on the real code.

Tool: property tests with generated inputs (proptest, pinned `=1.12.0`, dev-dependency). Rust has no cheap verifier for this code (it spawns processes and sockets), so the real `authorize`, `TokenTable::resolve`, `RunGrant` and `recall_args` run on generated token tables, request lines and peers. No hand model of any function.

## Properties (what, and the harm if it breaks)

All in `serve/host/src/runauth_props.rs`, one function each (`prop_*`), one `#[test]` each.

- P1 `prop_identity_is_the_tokens`: for any request, the identity `authorize` returns is exactly `resolve(token, peer)` and the run's own; forged `person`/`agent`/`room`/`project`/`audience`/`identity` fields or args never change it, for `memorySearch` and `projectId` too (the project answered is the token's `RunIdentity.project`, fixed where the token is issued). Harm: a guest reads another person's or the owner's memory.
- P2 `prop_foreign_peer_is_denied`: a run's token from a peer that is not that run's pid or below it (another run's process, a random pid, no pid, or before the token is bound) resolves to nothing, and the refusal is the same "not authorized". Harm: a token lifted from a neighbour's environment works.
- P3 `prop_revoked_and_unknown_are_denied`: a revoked token (run exited) and an unknown, empty, truncated, case-changed or garbage token are denied; revoking one run leaves the others working and the table exactly as long as the live runs. Harm: an exited run's token keeps reading.
- P4c/d/e `prop_memory_args_allow_list`, `prop_project_args_allow_list`, `prop_tool_args_caps_and_types`: `memory_args` returns `search` then only `-n <digits>` (clamped to 50), `--json` and the query words (`--all`, `--agent` and every other dash word refused); `project_args` allows no argument at all, or exactly `--info <the token's own project>` (`--list`, `--children`, `--where`, `--register`, `--root`, another id, a directory, a run with no project all refused); caps and types as P4b. Harm: a guest searches another agent's or the owner's memory, or lists or changes the project registry.
- P4 `prop_recall_args_allow_list` / `prop_recall_args_caps_and_types`: `recall_args` returns only the query words and the allow-listed narrowing flags, starting with `--tier` (so `index`/`forget` can never be a subcommand); every other dash argument (`--person`, `--agent`, `--audience`, `--env`, `-p`, `--room=x`, ...) is refused; no word dropped or invented; over-long, too many and non-string args refused. Harm: a guest sets who is asking, or reaches a write subcommand.
- P5 `prop_only_recall_is_authorized`: no method other than exactly `recall`, `memorySearch` and `projectId` is ever authorized (case, spaces, NUL, non-strings included), those three are (so refusing everything cannot pass), and an owner token (`TokenScope::OwnerAsk`) gets none of them. Harm: a guest runs a new or write method.
- P6 `prop_malformed_lines_are_denied`: bad JSON, random bytes, invalid UTF-8, a request over 64 KiB, deeply nested input, missing or mistyped `token`/`method`, non-objects are all denied, never a panic. Harm: a crafted line crashes or confuses the listener.
- P7 `prop_pid_reuse_and_sibling_runs`: a dead run's pid handed to a new run does not revive the dead token; two runs of one person in different rooms cannot use each other's tokens. Harm: a room's memory read from another room.

## Coverage record

| Function (serve/host/src/runauth.rs) | Properties |
|---|---|
| `TokenTable::resolve` (single token to identity path), `digest` | P1, P2, P3, P7 |
| `RunGrant::issue` / `bind_pid` / `Drop` (revoke) | P2 (unbound), P3, P7 |
| `authorize` | P1, P2, P3, P5, P6 |
| `recall_args` | P4, P1 and P5 (valid args only) |
| `memory_args`, `project_args` | P4c, P4d, P4e, P1, P5 |
| `descends` (real `ps` walk) | not here: the table's ancestry is injected (a pid window). `ancestry_follows_real_parents` in runauth.rs covers it |
| `handle_request`, `run_kernel`, `tool_path`, `SCRUB`, `serve_one`, `bind` | not covered by properties (spawn a process / need a socket); example tests in runauth.rs (`the_host_runs_the_kernel_tools_as_the_tokens_identity` runs a stand-in kernel) |
| `grant_for`, `grant_for_env` | not covered (need the global socket); example test `env_list_gets_a_token_only_with_a_person`. The project comes from `KITH_PROJECT` in the launch env (`agent/bot/hq.rs`: `launch.env`; `grant_for_env`: the env list), never from a request |

The two tool methods work the same way: `run_kernel` runs `bin/kith-memory search ...` or `bin/project-id --info ID` (found next to the `kith` entry), with `SCRUB` applied and `KITH_AGENT`, `KITH_PERSON`, `KITH_ROOM`, `KITH_VIA` (and, for the search, `KITH_PROJECT`) from the token, so `guest_run()` in kith-memory narrows to that person's memory. The bare `project-id` is answered from the token without running anything. Kernel side: `kernel/tests/kith-recall-check` has the guest-env checks (the request is only token, method, args; no folder is read). `bin/kith` unsets `KITH_RUN_TOKEN` for its own launch-time `project-id` calls.

How the token identity reaches the recall audience filter: `run_kernel` spawns the kernel's `kith recall` with `KITH_AGENT`, `KITH_PERSON`, `KITH_ROOM` set from `Authorized.identity` (everything else in `SCRUB` removed); kernel/lib/kith_recall.py `_caller()`/`caller()` turn those into the audiences the SQL `WHERE` allows (`shared`, `person:<id>`, `room:<id>`), on both the `index_rows` and the `fts` table. Properties P1 to P7 stop at `Authorized.identity` and the args: the env set-up and the SQL filter are NOT covered here. Cover them only with a new property over `run_recall` and a temp index.

## Owner runs

Owner runs get no token: `grant_for_env` issues one only when the env list names `KITH_PERSON`, and `agent/bot/hq.rs` only for a person or bot guest; the owner's `kith recall` reads the index directly. Runs that spawn helpers remove `KITH_RUN_TOKEN`/`KITH_RUN_SOCKET` (agent/acp.rs, agent/hq.rs). KITH_PERSON is never read on the host path: `authorize` and `resolve` take no env, so the property "env set or unset changes nothing" holds by construction (`run_recall` only removes it, then sets the token's person).

## A guest shell that drops its token

A guest's shell can unset `KITH_RUN_TOKEN` and `KITH_PERSON` (or set `KITH_PERSON` to the owner) and call the kernel tools directly. Those paths trust the env, so each one asks the OS too: `kith_paths.guest_sandbox()` is true when the home folder can't be listed (the guest sandbox's `denyRead` of the home; the owner's runs never have it), read from `pwd`, not `$HOME`. Inside it, no person or the owner means no one: `_caller()` in kith_recall.py (nobody), `thread_command` in kith-recall (no summarise), `guest_run()` in kith-memory (`-`), `acting()` in kith-auth (`-`), and `bin/kith` refuses to start an owner run. Outside it nothing changes. Checked by `kernel/tests/kith-recall-check` (Darwin, a `sandbox-exec` profile denying the home: each tool with the env dropped and with the owner named, and the same calls outside as controls) and by the `guest-sandbox` probe in `kith-guest-sandbox-check`'s `KITH_REAL_SANDBOX=1` run (the real Claude Code sandbox).

## Sync commit

Last confirmed that the properties describe `runauth.rs`, `kernel/bin/kith-recall`'s guest branch and the guest branches of `kith-memory` and `project-id` (`kernel/lib/kith_runsocket.py`): the commit that adds `memorySearch` and `projectId` (flow/guest-tools-via-host-runsocket, 2026-10-25; before it `7b6622a0`). When `git diff <sync> -- serve/host/src/runauth.rs kernel/bin/kith-recall kernel/bin/kith-memory kernel/bin/project-id kernel/lib/kith_runsocket.py` is not empty, re-read the diff against the coverage table before trusting a pass. Move the sync commit forward only after you have confirmed the properties still describe the code at the new commit (`/maintain-formal-verification-skill`).

## Run it

`cargo test --manifest-path serve/host/Cargo.toml --bin kith runauth_props` (CI: the host job runs it with the rest). `PROPTEST_CASES=2000` for a deeper local run. About a minute cold, seconds warm.

## Bounds and assumptions

- 1 to 5 runs per table; person/agent/room from small pools (4 people incl. a `bot-` id, 2 agents, 4 rooms incl. none) so values repeat across runs; 128 cases per property.
- Ancestry is a pid window (a peer descends from a run when it is 0 to 9 above the run's pid); the real `descends` is only example-tested.
- Peers come from a pid pool of 0..2000, `u32::MAX`, 0, 1 and none. Arguments are drawn from a fixed bank of known-dangerous words and flags plus random strings.
- Timing is not modelled: a token revoked between `authorize` returning and `run_recall` running still produces that one answer, as the person it was issued to. That is a check-then-act gap with no escalation (the identity is still the run's own); a model would only be needed if revocation had to cut off an answer in flight.

## NOT covered

- The OS sandbox that stops a guest reading the index or the data folder directly: kith-guest-isolation's smoke probe and the Linux CI check (#148).
- The recall SQL audience filter (kernel/lib/kith_recall.py), the env the host sets for the recall child, the real socket accept loop. `_caller()` is covered by examples only (kith-recall-check), not properties.
- A guest sandbox profile that allows reading the home folder: `guest_sandbox()` would then say owner. kith-guest-sandbox-check asserts the generated settings never allow it.
- Stale tokens held by a process outside the sandbox (an attacker with the owner's uid).

## Real bug or harness error

A failure prints the shrunk counterexample. It is a real bug when the minimal case, replayed by hand through `authorize(&table, line, peer)`, returns `Ok` with a wrong identity, or an arg the allow-list should refuse. It is a harness error when the failure is in `build`, `request` or a generator (a panic in test code, `unwrap` on `issue`, a strategy that cannot produce a valid case), or when `runauth.rs` changed intentionally (a new allowed flag in `VALUE_FLAGS`, a new method, a different refusal text): update the generator or expected text in the same change, never loosen a property to make it pass. A property that never saw a valid case is vacuous: P1, P5 and P6 assert the good path is accepted for that reason.

## Break table

Each property was shown failing on a broken variant of `runauth.rs` (temporary per-thread break switch, never merged; recorded from CI).

| # | Break (in runauth.rs) | Caught by |
|---|---|---|
| 0 | none (clean code) | nothing, all pass |
| 1 | `resolve` ignores the peer | P2, P7 |
| 2 | `revoke` is a no-op | P3, P7 |
| 3 | an unbound token matches any peer | P2 |
| 4 | `recall_args` treats dash words as query words | P4 |
| 5 | `recall_args` lets `--person` through as a flag | P4 |
| 6 | `authorize` accepts any method | P5 |
| 7 | identity taken from the request's `person` field | P1, P3 |
| 8 | a malformed line panics | P6 |
| 9 | the 64 KiB size limit removed | P6 |
| 11 | the argument-count cap removed | P4b |
| 12 | `authorize` skips `recall_args` | P1 |

Breaks for `memory_args` / `project_args` (a dash word let through, `--agent` allowed, `--info` of any id, an owner token accepted) are not recorded yet: the properties assert each of those directly, but they have not been run against a broken variant.

Read from the CI `host-test` job of PR #195 (a temporary commit added a thread-local `BREAK` switch read by `brk(n)` in runauth.rs and a meta-test running every property under every break; reverted afterwards). The switch is per thread, never a process-global env var, because tests run in parallel. To re-prove a property after changing it, redo that on a scratch branch: add `brk(n)` guards at the code named in the table, run each `prop_*` with the switch set, and expect exactly the callers listed. P4 once also failed on a clean run: the generator could make `-n`, a real flag, as a "bad" one. That was a harness error (fixed by filtering real flags out of the bad generator), not a bug.

## Keeping it up to date

When the covered code changes on purpose, update the properties with it and re-run the break table's idea on the new code. For the full routine, see `/maintain-formal-verification-skill`.
