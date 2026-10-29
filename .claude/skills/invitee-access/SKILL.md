---
name: invitee-access
description: Property tests that an invited person and a bot guest never get more than they were granted. Use when a change touches serve/host/src/api/roles.rs (guard, member_may, MEMBER_METHODS, BOT_METHODS, BOT_PENDING_METHODS, bot_may, shape, event_for, event_for_bot, bot_entry, shape_bot, hello_for, own_bot, owned_bot_chat_agent, approves_own, runs_code, stored_member_approver, may_use, may_use_checked, granted_agents), serve/host/src/api/botguests.rs (the five member handlers, end_bot, revoke_owned_by, enforce, bot_call, ask, the ten-bot limit), the bot principal in api/devices.rs (bot_principal), remote/botsync.rs, a router method in serve/host/src/api/ (a new method must stay off the member list unless justified), dispatch/member_call in api/mod.rs, people/proveInvite, the access-request approval (remote/cloud.rs decide/settle, accessRequests/decideAccessRequest in api/remote.rs), event fan-out in api/events.rs (events_stream, event_for), a guest's helpers (kernel/bin/kith-permit ask_helper), who answers a card (cards::approver_of, the run-card branch of guard, cards/access.rs escape cards), or kernel/bin/kith-auth (resolve, resolve_bot, may, person --rm/--revoke/--clear-temp, member and its env/mode checks, bot, bots-cap, access, forget, retired).
---

# Invitee access never exceeds the grant

The costliest bug in Kith is an invited person reaching an agent, chat, folder, project, event or permission prompt they were not granted. Unit tests check hand-picked cases; these check generated ones, on the real code.

Tool: property tests with generated inputs. Rust has no cheap verifier that runs async/subprocess code (Kani cannot), so the real `roles` functions run against a real store and a stand-in `kith-auth` (proptest, pinned `=1.12.0`, dev-dependency only). `kith-auth` itself is checked with hypothesis (pinned `6.169.2`, through `uv run --with`, nothing global).

## Properties (what, and the harm if it breaks)

Numbers are stable names, not an order (10-13 and 23-26 are kith-auth's, 14-20 the bot guests').

Host, `serve/host/src/api/roles_props.rs`:

1. `guard` lets a proven person through only for a method on `MEMBER_METHODS`; for a method that names a chat, that chat is theirs, not deleted, and kith-auth allows its agent right now. Harm: someone reads or drives another person's or the owner's chat.
2. An unproven device may only call `proveInvite`; a proven one that calls it again is refused with "already shown its invite" (the words the apps wait for). A member's device counts as proven when it shows the invite token or when the owner approved its access request by hand, on the Mac or from the owner's phone (`cloud::decide`, behind `decideAccessRequest`, which the owner's other devices may call only for a member's request); never when auto mode approved it, where nobody compares the code. Generated in two halves: `guard` here, and the approval in `serve/host/src/remote/cloud.rs` `a_members_device_is_proven_only_by_the_owners_own_approval` (any request, settled by the owner or by auto mode, yes or no: the device's `principal` is `Member` only for the owner's own yes, `Unproven` for auto mode's, nothing without the code or a live membership). Harm: an invite link alone, or the cloud alone, opens the host.
3. `respondPermission` passes only for a card of kind permission, raised by that chat, and only when kith-auth says preset `member`. Harm: a guest approves their own agent's dangerous action; a prompt of another chat is answered.
4. `send` passes with no folder or clear, and a project only if it is in the person's shared projects (checked with the handler's own `Change::from_args`). Harm: a guest picks a folder on the owner's Mac.
5. The same call after the grant is removed is refused (kith-auth is asked every call). Harm: a revoked person keeps access.
6. `event_for`: a person gets only `bot`/`entry`/`resync` events of their own chats whose agent is still in their person row, never usage, screens or cloud events, and no `cwd`/`command`/`folder`; the owner sees an invitee's chat only as permission prompts. In the stream (`events_stream`, before `event_for`), `accessRequests` reach this computer whole, the owner's other devices with members' requests only, and a member never (`events::tests::access_requests_reach_the_owners_devices_only_and_a_phone_only_members`, an example, not generated). Harm: leaks of other people's chats and of the Mac.
7. `shape` (hello, projects, history, thread, kithInfo), `shape_bot`, `hello_for` leave no folder, cwd, command, path or error text (also on a real `bot_json`). Harm: file paths of the Mac shown to a guest.
8. kith-auth fails closed: `may_use` is true only for `allowed: true` and not owner; `may_use_checked` is `Some(true)` exactly when `may_use` is true and `None` (never a yes) when kith-auth gave no answer or named the owner; `approves_own` never allows `Bash`; failure, empty or garbage output, `"true"` or `1` for allowed, truncated JSON are all a no; `approves_own` is true only for preset exactly `member`; `granted_agents` never lists an agent kith-auth denies. Harm: a broken kith-auth opens every door.
9. Calls that must pass do pass (`guard_lets_a_granted_member_use_their_own_chat`), so "refuse everything" cannot satisfy the rest.
21. The card of a `claude -p` run (a card-store row with source `run`) is answered by a person only when kith-auth says member now, the tool does not run code, and either the run was theirs and the host stored them as its approver, or the run was their bot guest's (docs/kith-bot-guests.md 7.6). For their bot's card, only what they could answer for a run of their own: not a file outside the bot's grant folders (F11 step 2a, `cards::approver_of` makes such a card the owner's) and not an escape card (`request_access`, F11 step 2b: its yes widens the bot's next activation). Harm: a member widens their bot's sandbox past what they hold, or allows a guest's shell.
22. In a chat (their own, or their bot guest's), `respondPermission` never passes for a tool that runs code (`runs_code`: kind `execute`, `Bash`, `mcp__*`, an unnamed tool of an unknown kind; `BashOutput`/`KillShell` are not code), nor for an entry stored with the owner as approver; and `respondPermission`, `react`, `editHandoff`, `keepConversation` never pass for a key or account card, whether the entry says so or its card-store row's tool is `request_secret`. Harm: a member allows the owner's shell on the Mac, or answers a key request.

Bot guests, host, `serve/host/src/api/botguests_props.rs` (the real `dispatch` against a real kith-auth in a temp folder, a temp store and a fake cloud: the `Rig` of `bot_guests_tests`; generated sequences of up to 8 operations, judged by a small model of who holds what):

14. A bot is served (`botAsk`) only while it is allowed in, was given the agent, and its owner holds that agent now; reading a conversation of its own (`botThread`) follows the same rule (pending: `pending`; ended: forbidden; active without the grant: `notGranted`). Harm: a bot reaches an agent its owner never had or no longer has, or reads answers of a turn that outlived a revoke.
15. A bot reaches only `BOT_METHODS`; one that waits for its owner's allow (`BotPending`) only `BOT_PENDING_METHODS` (hello, botStatus, proveBotInvite); an ended bot nothing. Checked by name for every router method, and by random calls. Harm: a bot calls a member's or the owner's method.
16. Taking the owner's agent (shrinking the grant, to zero too), removing the person, `revokeBot` or `botLeave` cut the bot at its next call, and the same call after that is refused again (also for a double revoke). The store state, the principal of the bot's key and `botStatus` follow the model. Harm: a removed person's bot keeps working.
17. The five member handlers (`createBotInvite` also for an existing bot, `revokeBotInvite`, `revokeBot`, `myBots`, `setBotLimits`) act only on the caller's own live bots, and `createBotInvite` only gives agents the caller holds; a person lowers limits to the defaults at most; `myBots` lists the caller's own bots and no others. Harm: a person manages another's bot or hands a bot more than they hold.
18. `event_for_bot` sends a bot the entries of its own conversations, and the entries and `turn` events of a group room it is a let-in member of (never another bot's conversation, a person's, the owner's private chat, another thread of its own chat, a room it is not in, another bot's `turn`, status, calls or usage), only while it is active (membership re-checked per event), shaped to words. Harm: a bot reads what it was never part of.
19. At most ten live bots per owner, counted per owner (the eleventh gets `tooMany`, another owner's bots do not count, removing one makes room). Harm: unbounded bots.
20. A bot never reads another bot's conversation (`botAsk`/`botThread` with its id is `notFound`).

kith-auth, `kernel/tests/kith-auth-props.py` (generated command sequences on a throwaway auth.json):

10. A person is allowed only through their own row or the `*` row; an unknown id never.
11. A bot is allowed only with a row of its own (the `*` row never reaches it) and while its owner is allowed right now; it runs as `guest`; for an invitee's bot, deny is a superset of the owner's, helpers are a subset, budget equals the owner's; the owner's bots take `owner_bots_budget` when set.
12. Removing a person removes their bots and every membership; they and their bots fail `resolve` at the next call. Taking an agent from a person takes it from their bots.
13. A refused `bots-cap` leaves the cap as it was.
23. (`access`, the always half of an escape card; C2) `kith-auth access AGENT WHO KIND VALUE` writes exactly when WHO is a person (not the owner, not removed or revoked) who holds the agent now through a row of their own, and, for a bot of an invitee, only for a place its owner holds now (a folder at or below one the owner holds, write covering read; a domain the owner lists); it writes only that row, a refusal writes nothing, and `--rm` takes the place away. A bot's resolved access never reaches past its owner's at the moment of the resolve, whatever its row still lists; a person allowed through the everyone row carries no access. Harm: an always answer widens a bot past its owner, or the everyone row widens everyone.
24. (F11 step 1) An allowed resolve never runs in a free-shell mode (`bypassPermissions`, `auto`) without `--trusted`, also when auth.json was hand-edited to say so. Harm: a guest's run gets a shell with no prompts.
25. (membership env) `member ... env.NAME=` refuses a name that changes how the run starts or which kith it talks to (`PATH`, `KITH_HOME`, `DYLD_INSERT_LIBRARIES`), writing nothing, and no allowed resolve carries one, hand edits included. Harm: a membership row runs the agent through another binary or another kith.
26. (forget) A new person or bot starts with nothing: no membership, grant or invite left behind by an earlier removal of the same id or a hand edit. Harm: an id handed out again inherits the old person's rights.
27. (retired, access part 6) Removing a person (not a bot) records their id in `retired`, and a retired id is never given to a new person or bot again. Harm: a new invite under the same name reads the old person's memory and files and uses their logins.

A guest's helpers, `kernel/bin/kith-permit` `ask_helper` (it runs `kith-ask` outside the guest sandbox) and the person gate of `kernel/bin/kith`, pinned by examples in `kernel/tests/kith-guest-helper-check`, not generated:

28. A helper never runs wider than its caller's membership (only a helper the membership names, and a helper cannot ask a third agent the caller's membership does not grant), never for a revoked person, never while this Mac is out of sync with the Kith cloud past the hold (`bin/kith` waits while kith-auth says `held`, then refuses; the agent is told to try again), and never with the owner's ask token. Harm: a guest reaches through a helper what their membership does not give them, or the owner's mailbox. The cross-Mac half (a revoke made elsewhere while held) is the revoke-access model's `R5c`.

## Coverage record

Sync commit: `fa36279b68fba905e19f57d5fd52347ab583696e` (2026-10-29: origin/main `bc3d92b8` plus the helper's in-sync wait and this re-sync's tests; `roles_props`, `botguests_props`, the property-2 and fan-out tests above and `kernel/tests/kith-auth-props.py` pass there). If `git diff fa36279b -- <file>` shows changes in a covered file, the check needs another look before you trust a pass.

Moved from `60d2031e` (2026-10-28): since then the covered files gained the owner-approved proven device and the phone's answer to members' requests (#382, #387: property 2 reworded and now generated), the `accessRequests` fan-out in `events_stream` and `accessRequests`/`decideAccessRequest` off `LOCAL_ONLY` (#387: property 6's stream sentence and its test; `decideAccessRequest` from a device other than this computer is refused for an owner device's request, `remote.rs`), the `proveInvite` branch of `guard` for a proven device (#378: property 2, generated), `ask_helper` for guests (#380: property 27), and in `cards/` rule 7 (#392: a card a `kith ask` run opened carries `reply_to`/`asked_by`, shown to the approver as a chain and answered back to the asker; who may answer is unchanged, so 21 and 22 stand); `kernel/bin/kith-auth` changed in comments only.

Recheck 2026-10-29 from `60d2031e`: the covered files changed by `proveInvite`'s "already shown" answer (roles.rs), access requests sent to owner devices (events.rs), the `LOCAL_ONLY` removal (devices.rs), the `reply_to`/`asked_by` fields of escape cards (cards/access.rs), and kith-auth `person --clear-temp` plus the temp folder removal in `--rm` and `--revoke`. None gives a person or bot more: the removal deletes a folder of theirs and grants nothing.

How it got there: first written at `5ced9454` (breaks run on `7b6622a0`); rechecked against `ed2ceb63`, where the properties and generators were brought up to the code (21-26 and the rows below are new) and property 21 failed on the code for the escape card and for Read/Edit outside the bot's folders. `6fc50729` (on main) bounded the bot owner's answer by the bot's grant folders, which fixed the Read/Edit cases; `60d2031e` refuses the bot owner its bot's escape card, which fixed the last one.

| File | Functions | Properties |
|---|---|---|
| serve/host/src/api/roles.rs | `guard` (incl. the proven `proveInvite` branch), `member_may`, `MEMBER_METHODS`, `UNPROVEN_METHODS` | 1, 2, 3, 4, 5, 9 |
| serve/host/src/remote/cloud.rs | `decide`, `settle` (`by_owner`: the proven device), `is_member_request`, `requests_json_for` | 2 |
| serve/host/src/api/remote.rs, api/devices.rs | `accessRequests`, `decideAccessRequest` (a device other than this computer: members' requests only), `LOCAL_ONLY` | 2, 6 (examples: `cloud::tests::the_owners_phone_sees_and_may_answer_only_members_requests`) |
| serve/host/src/api/events.rs | `events_stream`: the `accessRequests` fan-out, `LOCAL_EVENTS` | 6 (example) |
| | `own_bot`, `may_use`, `may_use_checked`, `resolve` (private), `approves_own`, `granted_agents` | 1, 3, 5, 8 |
| | `runs_code`, `stored_member_approver`, `owned_bot_chat_agent`, the run-card branch of `guard`, `secret_card` | 21, 22 |
| | `within`, `grant_roots` (the approver a card is stored with; the test home has no grant folders, so every named file is outside) | 21 (through `cards::approver_of`'s rule, written in the test) |
| serve/host/src/cards/access.rs | `TOOL` (`request_access`): an escape card on a run card | 21 |
| serve/host/src/api/calls.rs, api/cards.rs | their router methods (in the by-name sweeps of `roles_props`, `roles_tests`, `bot_guests_tests`) | 1, 15 |
| | `event_for`, `bot_person` | 6 |
| | `shape`, `shape_bot`, `hello_for` | 7 |
| serve/host/src/api/roles.rs (bots) | `BOT_METHODS`, `BOT_PENDING_METHODS`, `bot_may` | 15 |
| | `event_for_bot`, `bot_entry` | 18 (and `a_bot_guest_is_shown_words_only`) |
| serve/host/src/api/botguests.rs | `call`, `create_bot_invite`, `new_bot` (limit, agents held), `revoke_bot_invite`, `revoke_bot`, `my_bots`, `set_bot_limits`, `Actor::manages` | 17, 19 |
| | `end_bot`, `revoke_owned_by`, `enforce` (via `revokePerson`, `setPersonAccess`, `revokeBot`, `botLeave`) | 16 |
| | `bot_call`, `bot_method`, `ask`, `thread` (grant check at call time, conversation ownership), `status` | 14, 15, 16, 20 |
| serve/host/src/api/devices.rs | `bot_principal` | 15, 16 |
| serve/host/src/store/botguests.rs | `live_bot_count`, `bot_guest_of_device`, `end_bot_guest` (through the handlers) | 16, 19 |
| serve/host/src/agent/folder.rs | `Change::from_args` (used as the oracle for send) | 4 |
| serve/host/src/api/mod.rs | `member_call` (guard before route) | read, not run: see not covered |
| kernel/bin/kith-auth | `resolve`, `resolve_bot`, `may`, `bot_may`, `bot_row`, `cmd_person --rm`, `cmd_member`, `cmd_bot`, `cmd_bots_cap`, `retired`, `need_free` | 10-13, 27 |
| | `cmd_access` (C2), the access cap in `resolve_bot`, `check_env_key`, the `--trusted` free-shell rule, `forget`, `cmd_person --revoke/--unrevoke` | 12, 23-26 |
| kernel/bin/kith-permit, kernel/bin/kith | `ask_helper`; the person gate (`resolve`, the held wait) | 28 (examples) |

## Run

- Host, only these tests: `cd serve/host && cargo test --bin kith -- roles_props botguests_props a_members_device_is_proven access_requests_reach` (`--bin kith` builds the test binary once; without it cargo also builds the other targets; roles_props about 10 s of run time, botguests_props about 1.5 minutes because every case runs kith-auth as a subprocess; `PROPTEST_CASES=1000` for more, which is slow for the bot sequences). They run in the normal host `cargo test` in CI. The Mac is often loaded: prefer CI for the first compile.
- Helpers (27): `kernel/tests/kith-guest-helper-check` (a few seconds, plus about 3 s for the held wait).
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

### kith-auth breaks (2026-10-28, `kernel/tests/kith-auth-props.py` against a patched copy of `kernel/bin/kith-auth`, 300 generated examples, no shrinking)

Each is a one-text change to a copy of kith-auth in a scratch folder (the repo copy untouched); M0 is the unchanged copy, which passed. All of them failed `test_access_never_exceeds_the_grant` or the sequence property. Earlier generators let M1, M2, M6, M7 and M9 pass (no team of a person, their bot and memberships before the access calls; no `access` after a revoke or by the owner; no hand-edited free-shell mode after people existed): the TEAM opening, the pre-op of ACCESS and the second hand-edit list are why they fail now.

| # | The break | Caught by (shrunk case) |
|---|---|---|
| M1 | `resolve_bot` gives a bot its own `access` row uncapped | a bot reaches `write /work/c` its owner does not hold (23) |
| M2 | `cmd_access` skips the "its owner holds it now" check for a bot | `access house bot-a read /work/a/b` accepted (23) |
| M4 | `cmd_bot` does not `forget` a new bot | "new bot-b starts with an invite" (26) |
| M4b | `cmd_person` does not `forget` a new person | "new sam starts with a membership" (26) |
| M6 | `cmd_access` skips `may` (a revoked or removed person) | `access house amy write /work/c` accepted (23) |
| M7 | `cmd_access` skips `need_person` and the owner refusal | `access garden kai domain b.example` accepted for the owner (23) |
| M8 | `check_env_key` accepts `PATH` | `member garden kai env.PATH=x` accepted (25) |
| M9 | `mode_of` ignores `--trusted` | "sam runs house in auto without --trusted" (24) |
| M10 | `person --rm` does not record the id in `retired` (2026-10-29) | "sam: retired False" (27) |
| M11 | `cmd_person` skips `need_free` for a new person (2026-10-29) | `('person', 'sam')`: "a retired id was given to someone new" (27) |
| M13 | `retired()` ignores the `retired` table (2026-10-29) | `('person', 'sam')` (27) |

Not caught here: M12, `cmd_bot` skipping `need_free` (the generator makes bots only with `bot-*` ids, never a person's). `kernel/tests/kith-auth-c2-check` pins it ("a bot or a rename can't take it either"), with `--rm --unused` and the ids an older removal left memory or a profile for.

### Host breaks of the 2026-10-28 additions

Not run as breaks: property 21's run-card branch failed on the real code before `6fc50729` and `60d2031e` (finding 1 below, fixed), which shows the check can fail; the chat-card rules of 22 (`runs_code`, the stored approver, the card-store `request_secret` tool) are pinned by the hand-written cases 12-15 of `guard_lets_a_granted_member_use_their_own_chat`, and the `botThread` half of 14 has had no break run against it yet.

## Open findings

1. **Fixed 2026-10-28** (Read/Edit by `6fc50729`, the escape card by `60d2031e`: `their_bot` now also requires `within(grant_roots(bot person, agent), tool, input)` and a tool other than `request_access`; entries 16-18 refused). Kept for the record: **a bot guest's owner answered their bot's run cards past the bot's box** (property 21, `guard`'s run-card branch in `roles.rs`; seen failing 2026-10-28 at `ed2ceb63`). For a card whose run was their bot guest's, `guard` checks only `their_bot`, not code, and `approves_own(owner, agent, tool)`; it never looks at the approver the host stored (always `owner` for a bot's run: `cards::approver_of` asks `approves_own` of the bot, a guest), at `within`/`grant_roots`, or at the escape tool. Failing case, `guard_lets_a_granted_member_use_their_own_chat` entries 16-18: p1 is a member of a1 and owns bot guest `bot-c…g5`; that bot's `claude -p` run raised a run card (approver `owner`) for `request_access` (`read /Users/someone/Documents`), for `Read /etc/hosts`, and for `Edit /Users/someone/notes.txt`; `guard(p1, respondPermission, entry)` returns `Ok` for each. Harm: p1 could not answer any of these for their own run (they are stored as the owner's), but answers them for their bot: a yes to the escape card is applied as a once grant at the bot's next activation (`cards::access::answered`; even an "always" that kith-auth refuses becomes a once), so the bot's sandbox opens a folder or domain p1 does not hold; a yes to Read/Edit outside the grants removes the permission layer of F11 step 2a for the bot (the sandbox profile is the remaining layer). Introduced by `ccdd3f72` (run cards and the `their_bot` branch, 2026-10-24) and left as is by `b5da4481` (F11 step 2a: out-of-grant cards go to the owner) and the escape cards of `cards/access.rs`. The random draw of `guard_never_lets_a_person_past_their_grant` reaches this case too rarely (respondPermission, a run card of the bot person, its owner calling, a tool past the box: about 1 in 10^5 cases) and passed at 96 cases; the hand-written cases (added in `e9a1a46f`) are what fail. Fix in `guard`, not here: for `their_bot`, also require what the run's own member would need (the tool not `request_access`, and `within(grant_roots(bot person, agent), tool, input)`), or route these cards to the owner only.
2. **Liveness, not safety:** since `a46dab48` the ACP cards in a bot guest's chat store approver `owner`, so `stored_member_approver` refuses the bot's owner for every chat card of their bot, against docs/kith-bot-guests.md 7.6 ("An invitee owner's bot: Sam answers only when `approves_own(sam, agent)`"). Fails closed (the Mac owner still answers): `updates.rs` stores `approves_own(bot person, …)`, always `owner` for a bot, and `a46dab48` made the bot-chat branch of `guard` require a stored `member`. Not covered by a property that must pass.

## Not yet covered

- Bot guests: the rate limit (`ratePerHour`), the concurrency limit, the five-waiting queue, the seven-day silence, the ten-minute proof window and the proof tries (`PROVE_ATTEMPTS`) are pinned by example tests in `bot_guests_tests.rs`, not generated; `botsync.rs` (the cloud listing of keys, rotation) is exercised only as the way a generated bot joins; `enforce` stops running turns, which the rig cannot show; bots from before limits existed (limits 0) fail closed and are not generated; the Mac owner's `decideBot` / `setAutoAllowBots` are examples; a bot reaching `hello` shaped for it (`roles::shape`) is `a_bot_guest_is_shown_words_only`'s neighbour in `roles_props`, not part of the sequences.
- The cloud side of bot invites (token hashing, the listing of devices, TypeScript in serve/cloud) is out of scope.
- That owner devices and `Local` never reach `guard` (`dispatch` branches on `principal`): covered by `a_caller_is_the_owner_or_a_person` in `roles_tests.rs`, not generated.
- The handlers behind allowed methods (`upload`, `readUpload`, `react`, `history` payloads) beyond the shaping in `roles.rs`.
- A `kith:<name>` helper's MCP tools (it shares its caller's servers): `kith-auth helper-mcp`, the PreToolUse hook the plugin gives every helper (ask in the owner's session, deny in anyone else's, refuse when the check fails), is pinned by examples in `kernel/tests/kith-helper-mcp-check` and its registration in `kernel/tests/kith-hooks.test.ts`, not generated. That Claude Code runs a helper's own hooks (agent registration `hooks`) is assumed, as for `kith-helper-allow`.
- Card bounds (access-arch v3 P5, P6): `within` fails closed for a file tool that names no path (Grep, Glob with a relative pattern), a bot's owner answers the bot's run cards only inside the bot's own folders (`grant_roots` of the bot), and an app chat's (ACP) cards are bounded the same way (`acp_within`, used by `bot/updates.rs::on_permission`). Pinned by examples in `serve/host/src/cards/tests.rs` (`a_members_own_card_for_code_goes_to_the_owner`, `a_bots_owner_answers_its_run_cards_only_inside_the_bots_folders`), not generated by `roles_props`.
- A guest run's launch settings (`kernel/bin/kith`): the Bash allow hook for non-owner runs only, the `.work` workspace (its vars set for the sandboxed shell only, by `lib/guest-env.sh` through `CLAUDE_ENV_FILE`, and scrubbed by value from what `kith-auth exec` starts, with a working folder inside their folders moved to HOME), the Claude Code temp folder `<machine>/run/t/<p>` (readable and writable, `run/t` around it not; 0700; emptied when its record names someone else) and the `<limits>` note. Pinned by examples in `kernel/tests/kith-guest-sandbox-check`, not generated; that the OS sandbox refuses `/tmp`, `run/`, `run/t` and `host.sock` was seen on a real guest run (2026-10-29), not in CI.
- Cloud invite token hashing (serve/cloud, TypeScript), memory audiences (kith-context, design stage), one-writer threads (kith-formal-verify-threads).
