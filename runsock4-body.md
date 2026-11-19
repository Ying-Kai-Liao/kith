M2(c): guest run tokens are minted by the core's run.sock, not the host's TokenTable. Status: ready for architect code review; CI running.

## What changed
- Minting: `grant_for` / `grant_for_env` / `grant_for_room` (agent/bot/hq.rs, channels/run.rs, cards/resume.rs) mint only through the core (`run.mint` + `run.bind` + `run.end`). The `KITH_RUN_TOKENS_VIA_CORE` switch is gone. The host's `TokenTable` keeps the owner's cards tokens only. Core down or person revoked means no token (fail closed).
- host.sock stays, for the cards (`requestPermission` / `requestAccess` / `consumeAccess`, owner-only methods): it no longer holds a guest token. It asks the core `run.resolve` (new owner-socket row: token + connecting pid) for every guest request and refuses when the core says no or is down. `requestPermission` resolves again before it answers an allow (a revoke inside the card's live window turns the yes into a no: the run is denied, while the answered card still shows yes). `recall` / `memorySearch` / `projectId` are not host.sock methods any more; `recall_args` / `memory_args` / `project_args` / `run_kernel` left runauth.rs (1679 to 1300 lines; its tests moved to the core).
- The core answers the three tools (`core/runtime/src/runtools.rs`): `kith recall`, `kith-memory search`, `project-id` run as the token's identity (caller variables scrubbed and set from the token, own process group killed whole on the 30 s budget, output capped), outside the core lock between two `run_gate` checks, so a revoke in the middle drops the output. The kernel is found as the host finds it (`kith_core::set_kernel_finder(agent::hq::bin)`, asked on every call), never `<home>/kernel`.
- Room persons: `run.mint` takes `members`; `run_identity_allowed` checks the room person's id is of its room, the basis is not empty, every member may still have a run; `end_people` ends a room token when a member is named.
- Until the authority step is switched an id the core does not know (invited since the import) is accepted and a retired one (a removal) is not (architect MUST 1); once switched, known and not cut off.
- Guest side: the guest tools (`kith-recall`, `kith_runsocket.py` for kith-memory and project-id) use the new `KITH_CORE_RUN_SOCKET`; `KITH_RUN_SOCKET` stays host.sock (architect option A). A guest is the token alone: with no core socket the answer is empty, never the owner's index (`kith-recall`, `kith_runsocket.py`, `core/kernel/src/project`). `KITH_CORE_RUN_SOCKET` is in every launch-name and scrub list (host SCRUB lists, `terminal.rs`, `acp/client.rs`, `kith-core.sh`, `bin/kith` and `assemble.rs` ENV_NAMES).
- Sandbox: `allowUnixSockets` gets exactly `<machine>/core/run.sock` (both spellings) in `bin/kith` and `core/kernel/src/launch/settings.rs`; the `core` folder stays on `denyRead`.
- Docs: run-tokens skill, the CI `rooms` paths filter (names the core files the code moved to), plan log.

## Decisions (mine)
- `members` is stored on the core's RunIdentity and returned by `run.resolve`; the host's `RunIdentity.session` is the core's `conversation` (the host mints with `session_of(person, session)`).
- The kernel finder is a process-wide `set_kernel_finder` (a standalone `kith kernel core-daemon` sets it too); without it only `KITH_KERNEL_BIN` / `HQ_BIN` are read.

## Behaviour changes to know
- A daemon restart now also refuses a live guest's cards (not only recall) until its next turn mints again. Tokens minted by an older host (host.sock era) are nothing.
- `people.revoke` (one agent taken from one person) still does not end tokens; `run.mint` records no host generation (skill gates, not part of this step).

## Verification
Ran:
- `cargo test -p kith-core --lib -- runsock_tests runs_props authority::tests::runs`: pass (incl. 5 new real-socket tests in `runsock_tools_tests.rs`)
- `cargo test --bin kith -- runauth core_front hq_person cards::`: pass after the one fix pushed (229 tests)
- `cargo clippy -p kith-core -p kith-api -p kith-kernel -p kith-acp -p kith-serve --all-targets --locked -- -D warnings`: pass; `cargo fmt` on the five crates
- see the CI run linked in the report for the full set
Exercised: macOS sandbox via Claude Code's sandbox runtime (srt) with this branch's generated guest sandbox block: connect to core/run.sock ok, run/host.sock ok, core/owner.sock denied, cat core/owner.token denied, ls core denied. A launchd job with bare PATH ran `kith recall zebra --json` as a guest identity: `[]`, exit 0 (kernel copied to /tmp; launchd cannot exec from the external disk). Also: real daemon over real sockets in `runsock_tools_tests.rs` (recall, memorySearch, projectId --info as the token's identity through run.sock; revoke in the middle of a recall; room basis; `run.resolve`; daemon restart) and `core_front` mint + resolve through a real daemon.
Not verified:
- a full `claude -p` run under KITH_REAL_SANDBOX=1 (hung in this environment; the srt run stands in)
- a real slow core (only an injected resolver in a unit test)
- host test agent::bot::writer::tests::a_fired_cron_job_and_a_fired_routine_each_run_once_and_end_done failed locally ("Routines are run by the Kith core now"); unrelated to run tokens, CI decides
- Windows (clippy by reading cfg gates only)

🤖 Generated with [Claude Code](https://claude.com/claude-code)

