---
name: invitee-access
description: Property tests that an invited person and a bot guest never get more than they were granted. Use when a change touches serve/host/src/api/roles.rs (guard, member_may, MEMBER_METHODS, BOT_METHODS, BOT_PENDING_METHODS, bot_may, shape, event_for, event_for_bot, bot_entry, shape_bot, hello_for, own_bot, approves_own, may_use, granted_agents), serve/host/src/api/botguests.rs (the five member handlers, end_bot, revoke_owned_by, enforce, bot_call, ask, the ten-bot limit), the bot principal in api/devices.rs (bot_principal), remote/botsync.rs, a router method in serve/host/src/api/ (a new method must stay off the member list unless justified), dispatch/member_call in api/mod.rs, people/proveInvite, event fan-out in api/events.rs, or kernel/bin/kith-auth (resolve, may, person --rm, member, bot, bots-cap).
---

# Invitee access never exceeds the grant

The costliest bug in Kith is an invited person reaching an agent, chat, folder, project, event or permission prompt they were not granted. Unit tests check hand-picked cases; these check generated ones, on the real code.

Tool: property tests with generated inputs. Rust has no cheap verifier that runs async/subprocess code (Kani cannot), so the real `roles` functions run against a real store and a stand-in `kith-auth` (proptest, pinned `=1.12.0`, dev-dependency only). `kith-auth` itself is checked with hypothesis (pinned `6.169.2`, through `uv run --with`, nothing global).

## Properties (what, and the harm if it breaks)

Host, `serve/host/src/api/roles_props.rs`:

1. `guard` lets a proven person through only for a method on `MEMBER_METHODS`; for a method that names a chat, that chat is theirs, not deleted, and kith-auth allows its agent right now. Harm: someone reads or drives another person's or the owner's chat.
2. An unproven device may only call `proveInvite`. Harm: an invite link alone opens the host.
3. `respondPermission` passes only for a card of kind permission, raised by that chat, and only when kith-auth says preset `member`. Harm: a guest approves their own agent's dangerous action; a prompt of another chat is answered.
4. `send` passes with no folder or clear, and a project only if it is in the person's shared projects (checked with the handler's own `Change::from_args`). Harm: a guest picks a folder on the owner's Mac.
5. The same call after the grant is removed is refused (kith-auth is asked every call). Harm: a revoked person keeps access.
6. `event_for`: a person gets only `bot`/`entry`/`resync` events of their own chats whose agent is still in their person row, never usage, screens or cloud events, and no `cwd`/`command`/`folder`; the owner sees an invitee's chat only as permission prompts. Harm: leaks of other people's chats and of the Mac.
7. `shape` (hello, projects, history, thread, kithInfo), `shape_bot`, `hello_for` leave no folder, cwd, command, path or error text (also on a real `bot_json`). Harm: file paths of the Mac shown to a guest.
8. kith-auth fails closed: `may_use` is true only for `allowed: true` and not owner; failure, empty or garbage output, `"true"` or `1` for allowed, truncated JSON are all a no; `approves_own` is true only for preset exactly `member`; `granted_agents` never lists an agent kith-auth denies. Harm: a broken kith-auth opens every door.
9. Calls that must pass do pass (`guard_lets_a_granted_member_use_their_own_chat`), so "refuse everything" cannot satisfy the rest.

Bot guests, host, `serve/host/src/api/botguests_props.rs` (the real `dispatch` against a real kith-auth in a temp folder, a temp store and a fake cloud: the `Rig` of `bot_guests_tests`; generated sequences of up to 8 operations, judged by a small model of who holds what):

14. A bot is served (`botAsk`) only while it is allowed in, was given the agent, and its owner holds that agent now. Harm: a bot reaches an agent its owner never had or no longer has.
15. A bot reaches only `BOT_METHODS`; one that waits for its owner's allow (`BotPending`) only `BOT_PENDING_METHODS` (hello, botStatus, proveBotInvite); an ended bot nothing. Checked by name for every router method, and by random calls. Harm: a bot calls a member's or the owner's method.
16. Taking the owner's agent (shrinking the grant, to zero too), removing the person, `revokeBot` or `botLeave` cut the bot at its next call, and the same call after that is refused again (also for a double revoke). The store state, the principal of the bot's key and `botStatus` follow the model. Harm: a removed person's bot keeps working.
17. The five member handlers (`createBotInvite` also for an existing bot, `revokeBotInvite`, `revokeBot`, `myBots`, `setBotLimits`) act only on the caller's own live bots, and `createBotInvite` only gives agents the caller holds; a person lowers limits to the defaults at most; `myBots` lists the caller's own bots and no others. Harm: a person manages another's bot or hands a bot more than they hold.
18. `event_for_bot` sends a bot the entries of its own conversations only (never another bot's, a person's, the owner's private chat, another thread of its own chat, status, calls or usage), only while it is active, shaped to words. Harm: a bot reads what it was never part of.
19. At most ten live bots per owner, counted per owner (the eleventh gets `tooMany`, another owner's bots do not count, removing one makes room). Harm: unbounded bots.
20. A bot never reads another bot's conversation (`botAsk`/`botThread` with its id is `notFound`).

kith-auth, `kernel/tests/kith-auth-props.py` (generated command sequences on a throwaway auth.json):

10. A person is allowed only through their own row or the `*` row; an unknown id never.
11. A bot is allowed only with a row of its own (the `*` row never reaches it) and while its owner is allowed right now; it runs as `guest`; for an invitee's bot, deny is a superset of the owner's, helpers are a subset, budget equals the owner's; the owner's bots take `owner_bots_budget` when set.
12. Removing a person removes their bots and every membership; they and their bots fail `resolve` at the next call. Taking an agent from a person takes it from their bots.
13. A refused `bots-cap` leaves the cap as it was.

## Coverage record

Sync commit: `5ced9454` (origin/main when written; the covered files below are unchanged since `7b6622a0`, the commit the breaks were run on). If `git diff 5ced9454 -- <file>` shows changes in a covered file, the check needs another look before you trust a pass.

| File | Functions | Properties |
|---|---|---|
| serve/host/src/api/roles.rs | `guard`, `member_may`, `MEMBER_METHODS`, `UNPROVEN_METHODS` | 1, 2, 3, 4, 5, 9 |
| | `own_bot`, `may_use`, `resolve` (private), `approves_own`, `granted_agents` | 1, 3, 5, 8 |
| | `event_for`, `bot_person` | 6 |
| | `shape`, `shape_bot`, `hello_for` | 7 |
| serve/host/src/api/roles.rs (bots) | `BOT_METHODS`, `BOT_PENDING_METHODS`, `bot_may` | 15 |
| | `event_for_bot`, `bot_entry` | 18 (and `a_bot_guest_is_shown_words_only`) |
| serve/host/src/api/botguests.rs | `call`, `create_bot_invite`, `new_bot` (limit, agents held), `revoke_bot_invite`, `revoke_bot`, `my_bots`, `set_bot_limits`, `Actor::manages` | 17, 19 |
| | `end_bot`, `revoke_owned_by`, `enforce` (via `revokePerson`, `setPersonAccess`, `revokeBot`, `botLeave`) | 16 |
| | `bot_call`, `bot_method`, `ask` (grant check at call time, conversation ownership), `status` | 14, 15, 16, 20 |
| serve/host/src/api/devices.rs | `bot_principal` | 15, 16 |
| serve/host/src/store/botguests.rs | `live_bot_count`, `bot_guest_of_device`, `end_bot_guest` (through the handlers) | 16, 19 |
| serve/host/src/agent/folder.rs | `Change::from_args` (used as the oracle for send) | 4 |
| serve/host/src/api/mod.rs | `member_call` (guard before route) | read, not run: see not covered |
| kernel/bin/kith-auth | `resolve`, `resolve_bot`, `may`, `bot_may`, `bot_row`, `cmd_person --rm`, `cmd_member`, `cmd_bot`, `cmd_bots_cap` | 10-13 |

## Run

- Host, only these tests: `cd serve/host && cargo test --bin kith roles_props botguests_props` (roles_props about 10 s of run time, botguests_props about 1.5 minutes because every case runs kith-auth as a subprocess; `PROPTEST_CASES=1000` for more, which is slow for the bot sequences). They run in the normal host `cargo test` in CI. The Mac is often loaded: prefer CI for the first compile.
- kith-auth: `kernel/tests/kith-auth-props` (seconds; `PROPS_EXAMPLES=3000` for more). Needs `uv`. CI runs it as the "Auth property check" step of the kernel job, right after "Auth check".

## Bounds and assumptions

- The stand-in kith-auth (`$KITH_FAKE/<person>__<agent>` files, a shell script) answers `resolve AGENT --person P` only; a missing file is a failure (exit 1, nothing printed). It assumes the real kith-auth prints its answer on stdout as one JSON object; the host ignores the exit status, so a failing process that printed `allowed: true` is not modelled.
- Seam: `account::TEST_BIN`, `hq::TEST_ENV` and `ONE_AT_A_TIME` (all `cfg(test)`); no product code was changed.
- Two people, two agents, four chats plus one the store never had, up to three entries, two projects; method names from the router walk (copied from `roles_tests::router_methods`), `MEMBER_METHODS` and random text. 96 cases per property by default.
- Events are checked by their top-level fields; a nested secret in an `entry` payload is content, not covered. `resync` passes unchanged by design.
- The person row (agents, projects) lives in the host store and is judged by `event_for`; kith-auth is judged by `guard`. The harness generates them independently, so a drift between them is allowed by the generator and only the code decides.
- Bot guests use the real kith-auth through `Rig` (not the stand-in), one kith-auth and store per case; the cloud is the rig's fake (invite posts and the listing of bot keys). Two invited people (sam, ann, both members of house and money), Kai the computer's owner with owner chats of both agents, up to 8 generated operations plus a fixed opening (a bot each) and a fixed ending (ann loses her agents, sam is removed); sweeps by name check every router method and every cross-owner call on every bot. Rate and concurrency limits are not run (deterministic, but already pinned by `limits_and_ownership_hold`); the model's expectations of who holds what are written from the operations, not read back from kith-auth.
- The bot sequence property keeps its checks cheap enough for the 300 s per-test limit of CI: 10 cases, shrinking limited to 24 steps.
- Python side: two invitees, two bots, two agents, sequences up to ~19 commands through `kith-auth main()` in process; commands the CLI refuses are skipped (their effect is checked only where stated).

## Real bug or harness error

A failure prints a shrunk case. Reproduce it by hand: write the person and bot rows as the scenario says, put the answer files in a temp dir, call the function. If the code lets the call through while the scenario says it should not, it is a bug: do not weaken the property; report it with the steps. If the scenario cannot happen in the product (for example a kith-auth answer it never prints), fix the generator and say why in this file. A panic in `World::new` or a script error is the harness.

## Updating for an intended change

- A new `MEMBER_METHODS` entry: add it with a reason in `roles.rs`, then say here which guard branch covers it; if it names a chat by a new field, teach `by_entry`/`body` in `roles_props.rs` that field, or property 1 passes by never exercising it.
- A new event type, shaped answer or bot-guest role (`BOT_METHODS`, bot principal): add a generator and a property, then a broken variant that fails it.
- kith-auth rules: change the Python oracle in the same commit as the rule.
- Move the sync commit only after you confirm the properties still describe the code at the new commit (re-read the covered functions, run both commands, and for each new branch see a broken variant fail). `/maintain-formal-verification-skill` (pstack) brings this in line after changes that did not recheck it.

## Broken variants seen failing

Each break below was a one-line change in the product code behind a thread-local switch (a temporary commit on #194, run on CI, then reverted; the switch is per test thread, never a process-wide variable, and break 0 is the clean code, which passed all properties). "Caught by" lists every property that failed on that break in the last CI run; the first is the one the break targets. Breaks 1-28 are in `roles.rs`; 29-50 in `roles.rs`, `botguests.rs`, `devices.rs` and `store/botguests.rs`. Single-method breaks used to slip by a random draw (breaks 1, 4, 5, 7, 8, 13, 14, 28, 32, 36, 42, 43, 47, 49): the properties now also run them by name or by a fixed opening and ending, which is why a few properties appear twice.

| # | The break | Caught by |
|---|---|---|
| 1 | a method off `MEMBER_METHODS` passes `guard` | `guard_never_lets_a_person_past_their_grant`, `guard_lets_a_granted_member_use_their_own_chat` (the by-name sweep) |
| 2 | an unproven device may call more than `proveInvite` | `guard_never_lets_a_person_past_their_grant` |
| 3 | `own_bot` ignores whose chat it is | `guard_never_lets_a_person_past_their_grant`, `guard_lets_a_granted_member_use_their_own_chat` |
| 4 | `own_bot` lets a deleted chat through | `guard_lets_a_granted_member_use_their_own_chat` |
| 5 | `own_bot` skips the live kith-auth check | `guard_lets_a_granted_member_use_their_own_chat`, `guard_never_lets_a_person_past_their_grant` |
| 6 | a guest answers the prompt of their run | `guard_lets_a_granted_member_use_their_own_chat` |
| 7 | `respondPermission` accepts a card that is not a permission | `guard_lets_a_granted_member_use_their_own_chat` |
| 8 | `respondPermission` accepts a card another bot raised | `guard_lets_a_granted_member_use_their_own_chat` |
| 9, 10, 11 | `send` takes a folder, `clear`, or an unshared project | `guard_lets_a_granted_member_use_their_own_chat` |
| 12 | an event of another person's chat reaches a person | `events_reach_only_the_person_they_are_about` |
| 13 | a chat of an agent the person lost still reaches them | `a_real_bot_json_loses_where_it_works`, `events_reach_only_the_person_they_are_about` |
| 14 | an invitee's chat events reach the owner whole | `a_real_bot_json_loses_where_it_works`, `events_reach_only_the_person_they_are_about` |
| 15 | usage, screens and cloud events reach a person | `events_reach_only_the_person_they_are_about` |
| 16 | a bot event is not shaped for a person | `a_real_bot_json_loses_where_it_works` |
| 17, 18, 19 | `hello` keeps `cwd`; `projects` keeps the error text; `history` keeps the folder | `what_a_person_is_shown_has_no_paths_folders_or_error_text` |
| 20 | `projects` lists unshared projects | `what_a_person_is_shown_has_no_paths_folders_or_error_text` |
| 21, 22 | `shape_bot` keeps `cwd`, `command` | `what_a_person_is_shown_has_no_paths_folders_or_error_text`, `a_real_bot_json_loses_where_it_works` |
| 23 | `hello_for` adds `cwd` | `what_a_person_is_shown_has_no_paths_folders_or_error_text` |
| 24 | `may_use` true for any answer kith-auth printed | `kith_auth_answers_fail_closed`, `guard_never_lets_a_person_past_their_grant` |
| 25 | the owner answer passes `resolve` | `kith_auth_answers_fail_closed` |
| 26 | `approves_own` ignores the preset | `kith_auth_answers_fail_closed`, `guard_lets_a_granted_member_use_their_own_chat` |
| 27 | `granted_agents` lists denied agents | `kith_auth_answers_fail_closed` |
| 28 | `member_may` accepts `createBot` | `guard_lets_a_granted_member_use_their_own_chat` (the by-name sweep) |
| 29 | `bot_may` and `bot_method` let a bot call `send`/`history` | `a_bot_never_has_more_than_its_owner_holds_now` (15) |
| 30 | a waiting bot may call any bot method | same (15) |
| 31 | a pending bot's key is an active `Bot` principal | same (15, 16) |
| 32 | `revoke_owned_by` does nothing | same (16) |
| 34 | `botAsk` skips the live grant check | same (14) |
| 35 | `Actor::manages` lets any member manage any bot | same (17) |
| 36 | a member may give a bot an agent they do not hold | same (17) |
| 37 | the cap is eleven | `a_bot_owner_has_at_most_ten_live_bots` (19) |
| 38 | the cap counts the computer owner's bots for everyone | `a_bot_owner_has_at_most_ten_live_bots` (19) |
| 39, 40, 41 | `event_for_bot` drops the conversation owner check; the active check; the word shaping | `a_bot_is_sent_only_its_own_conversations` (18) |
| 42 | `myBots` lists others' bots | `a_bot_never_has_more_than_its_owner_holds_now` (17) |
| 43 | a person may raise their bot's limits past the defaults | same (17) |
| 44 | the store finds an ended bot by its key and the device is not removed | same (16) |
| 47, 48, 49, 50 | `revokeBotInvite`, `setBotLimits`, `revokeBot`, `createBotInvite{botId}` skip the owner check | same (17) |

Not shown failing (no single change reaches them): breaks of `enforce` (it stops running turns, which the rig has none of; the next call is cut by the kith-auth check in `ask`, broken in 34), of `bot_principal`'s state arms alone (the device is removed, the store query skips ended bots: three walls, break 44 takes all three down), and of the second wall `bot_method`'s default arm (break 29 takes both down). Break 33 and 45, 46 were dropped for the same reason or as not observable.

## Not yet covered

- Bot guests: the rate limit (`ratePerHour`), the concurrency limit, the five-waiting queue, the seven-day silence, the ten-minute proof window and the proof tries (`PROVE_ATTEMPTS`) are pinned by example tests in `bot_guests_tests.rs`, not generated; `botsync.rs` (the cloud listing of keys, rotation) is exercised only as the way a generated bot joins; `enforce` stops running turns, which the rig cannot show; bots from before limits existed (limits 0) fail closed and are not generated; the Mac owner's `decideBot` / `setAutoAllowBots` are examples; a bot reaching `hello` shaped for it (`roles::shape`) is `a_bot_guest_is_shown_words_only`'s neighbour in `roles_props`, not part of the sequences.
- The cloud side of bot invites (token hashing, the listing of devices, TypeScript in serve/cloud) is out of scope.
- That owner devices and `Local` never reach `guard` (`dispatch` branches on `principal`): covered by `a_caller_is_the_owner_or_a_person` in `roles_tests.rs`, not generated.
- The handlers behind allowed methods (`upload`, `readUpload`, `react`, `history` payloads) beyond the shaping in `roles.rs`.
- Card bounds (access-arch v3 P5, P6): `within` fails closed for a file tool that names no path (Grep, Glob with a relative pattern), a bot's owner answers the bot's run cards only inside the bot's own folders (`grant_roots` of the bot), and an app chat's (ACP) cards are bounded the same way (`acp_within`, used by `bot/updates.rs::on_permission`). Pinned by examples in `serve/host/src/cards/tests.rs` (`a_members_own_card_for_code_goes_to_the_owner`, `a_bots_owner_answers_its_run_cards_only_inside_the_bots_folders`), not generated by `roles_props`.
- Cloud invite token hashing (serve/cloud, TypeScript), memory audiences (kith-context, design stage), one-writer threads (kith-formal-verify-threads).
