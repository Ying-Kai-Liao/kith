---
name: secret-cards
description: Model-checks (TLA+/TLC) that a secret card (a Key or Access card, an agent asking the owner for a service's key) writes the Keychain at most once, only while the card is open, never after a no or the clock closed it, and that a failed write leaves the card open. Use when a change touches serve/host/src/cards/secret.rs (provide, respond, deny, grant, Slot, write_key), the card store's claim and closers in serve/host/src/store/cards.rs (card_claim, card_claim_release, card_saved, card_answer, cards_sweep, card_withdraw), the closers in serve/host/src/cards/mod.rs, provideSecret / listKeys / revokeKey in serve/host/src/api/cards.rs, the secret routing of respondPermission in api/bots.rs or roles.rs, or kith-auth set --stdin-json / grant / keys / revoke --profile; or the core's version: core/runtime/src/cards/mod.rs (claim, release, saved, saved_in, withdraw, sweep, park_run, answer, void_dead_claims, the pid-held claim) and cards/secret.rs (begin, commit, provide, the cards.provide row), core/runtime/src/secrets/ (the envelope put/get it uses), core/api/src/table/cards.rs. Also use to answer "can a key be saved twice, or after the owner said no?".
---

# Secret cards: one write per card, never after it closed

An agent that needs a service's key asks the owner (`requestSecret` on the run socket, or the chat tool
`request_secret`). The owner gets a card in their chat with the agent: an **Access card** (pick an account the owner
already has: `respondPermission profile:<name>` -> `kith-auth grant`) or a **Key card** (type the key into the app's
secure field: `provideSecret` -> `kith-auth set <profile> --stdin-json --grant <agent>`). The architect's design is
`designs/2026-10-25-secret-cards.md` (not in this repo; `cards/secret.rs` cites it).

Two of the owner's devices can answer the same card at once, the clock can expire it, and the owner can say no, all
while a key is being written. Whether a key can land twice, or land after the card was closed, is a question of the
order of those steps, so it is checked with TLA+ (TLC). The tests (below) check each step on the code; the model
checks that the steps add up.

The model is `verification/secrets/Secrets.tla`. This skill is its only documentation.

## Run it

```
verification/secrets/run.sh                          # every row of checks.tsv (26 rows, about a minute)
verification/secrets/run.sh --show VARIANT PROPERTY   # one row with TLC's full output (the counterexample)
verification/secrets/run.sh --list                   # the rows
verification/secrets/drift.sh                        # covered files changed since the sync commit?
```

Same pinned tools as `verification/threads/run.sh` (asked for with `--tools`): `tla2tools.jar` v1.7.4, Temurin JRE 21
only when no Java 11+ is present, cached under `${XDG_CACHE_HOME:-$HOME/.cache}/kith-verify/`. Each row writes a
`Secrets.cfg` with the variant's three switches and one property. Exit 0 only when every row came out as `checks.tsv`
expects. CI runs it in the `secret-cards` job (`.github/workflows/ci.yml`) when a covered file changes.

The tests of each step on the code:

```
cd serve/host && cargo test cards::secret     # cards/secret/tests.rs: two devices write once, the clock can't expire
                                              # a card mid-write, a failed write leaves it open, a closed card writes
                                              # nothing, a no ends it, only the owner provides/lists/revokes, the key
                                              # reaches only kith-auth's stdin
cd serve/host && cargo test a_secret_card_is_written_at_most_once   # cards/props.rs: random claim/finish/deny/sweep on
                                              # the card store (the slot is the test's own flag there)
kernel/tests/kith-auth-secret-check           # kith-auth: set --stdin-json, account-state, keys, revoke --profile
```

The core's version (M2-12, `core/runtime/src/cards/secret.rs`) is tested in `core/runtime/src/cards/secret/tests.rs` on CI
(`kith-core`, the real envelope over a fake `MemoryKeyStore`; a trigger on `secret_records` logs every committed write
and fails one on demand): a key is sealed under its record name and the card ends saved; a second provide, a closed,
denied, withdrawn, expired or unswept-expired card writes nothing; a failed write or a locked key store leaves the card
open with the claim released and no half a key; a second device is refused while the first holds the claim; a writer
that lost its claim writes nothing; a claim that ran out by time is not taken over while its writer's pid lives
(precondition 1); the claim carries this process's pid and start time (precondition 2); a parked card is provided and a
held claim survives park_run and withdraw; a claim that could not be released is freed at the next row call; a damaged record does not block the warm-up and an account is taken by its `key/<service>/<account>/` prefix; eight threads, serialised behind one Core as the daemon does, write one card once (so contention is decided in the rows, not by real parallelism: a second Core on one folder cannot exist); and a random sequence (provide, no,
sweep, park_run, withdraw, a writer in flight, a crash, the clock) checks at-most-once, nothing-after-close and
written-iff-saved after every step.

## Sync commit

**f5b92e19daddcfdfc92162abfc38be2b63dd3e4d** (2026-11-12, M2-23 S-b: kith-auth set/grant/revoke/env/connect/disconnect route to the core on the tombstone, Keychain first then core; the secret-cards model gained the `switched*` rows (the Keychain is written at most once while the card is open; a core failure leaves an orphan item only); `run.sh` every row as expected.) Earlier: **732dfef5fa4a31a190277b2ffb02895d3da3ff1a** (2026-11-04, M2-12: the core's secret cards, `core/runtime/src/cards/{mod,secret}.rs` over `secrets/`, covered from here; the model gained `Withdraw`, `Crash`, `Lapse`, `Commit` and three switches, 26 rows pass. The host rows gained `Withdraw` too; it found a host gap, fixed in the same change: the host's `card_withdraw` (from `delivered` and `withdraw_entry`) closed a card whose key was being written. Both callers now skip a card whose `Slot` is held, `secret::claimed`; test `a_withdrawal_waits_for_a_key_being_written`). Before that: **0872ea58892e0f648d4206bd64e455b25cdf2d5c** (2026-11-03: rust-rebuild at 0872ea58892e with F5 (kith-auth refuses built-in agents; narrower only). Re-run: `run.sh` every row as expected (14 rows), `cargo test cards::secret` and `a_secret_card_is_written_at_most_once` pass, `kith-auth-secret-check` passes. Read since the last sync: `secret.rs` gained the holder redirect, `openKeyCard` (an owner-opened card through the same `card_open`/provide write path), `answerable_by` and the card lane; the claim, slot, `write_key` and deny steps are unchanged. The resume claim added in `store/cards.rs` is a different claim from the write claim. Earlier: **b241d7bbd3372fb6bd31bf1024c10b179ef2645b** (origin/main on 2026-10-28; first written at ed2ceb63, after the secret
cards, host d8713608 and kernel 7863e2e9, and the limit cards, adcfb2f7, which only add a branch after the secret
routing in `respondPermission`). Between the two, the covered files changed only around access cards (the escape
card's once grants in `store/cards.rs`, `cards::answer` writing the entry first), the grants ledger and other
commands of kith-auth, and the bot's file bound in `guard`; no step of a secret card. The same sha is in `drift.sh`
(`SYNC=`). Move it only after you have confirmed that the model still describes the covered code (see
"When you change covered code").

## What is promised, and the harm if not

| Property (Secrets.tla) | Plain sentence | Harm if it breaks |
|---|---|---|
| `WriteWhileOpen` | Every write of the Keychain for a card happens while the card is open. | A key is saved and granted to the agent for a request that had already ended. |
| `ClosedNeverWritten` | A card that was denied or expired was never written. | The owner said "Not now" (or let it lapse), and the key is saved and granted to the agent anyway. |
| `AtMostOneWrite` | At most one write per card. | Two devices both save: the second key silently replaces the first, or two accounts are made, while the owner believes one was. |
| `WrittenEndsSaved` | Once nothing is in flight, a written card is saved (spent, its resume owed). | The key is in the Keychain but the card shows open or expired and the agent is never told; the owner types it again. |
| `FailLeavesOpen` | A failed write changes nothing the card shows: it stays open and unwritten by that step. | A failed save closes the card; the owner can't retry and the agent never gets the key. |
| `TypeOK` | The variables stay in range. | |

## Coverage record (code -> model action)

| Step in the code | Code | Model action |
|---|---|---|
| take the right to answer: the in-memory slot, then the store's write claim (open, before the deadline; the deadline pushed out `CLAIM_MS` for the write) | `cards/secret.rs` `Slot::take`, then `store/cards.rs` `card_claim`, in `provide` (Key card) and `grant` (Access card) | `Claim(d)` (one step: each half refuses on its own, and a no takes the same slot) |
| write the Keychain and the grant | `write_key` -> `kith-auth set <profile> --stdin-json --grant <agent>` (`cmd_set_stdin`); or `kith-auth grant <agent> <service> <profile>` | `Write(d)` |
| the write failed: claim released to the card's own deadline, card open | `card_claim_release(id, claimed.deadline)`, then the slot drops | `Fail(d)` |
| the card is answered after the write | `card_saved` (only an open card: `state IN ('pending','live','parked')`), then the slot drops, `finish` | `Finish(d)` |
| claim (core): an open card before its deadline, nobody holding it; the holder is unique to the call, the claim has the daemon's pid and start time and lasts `CLAIM_MS` (5 min) | `cards/secret.rs` `begin` -> `cards/mod.rs` `claim` (`void_dead_claims` first) | `Claim(d)` |
| write and save (core): ONE transaction checks the claim is still ours and the card open, seals every value (record name as associated data) and spends the card; a failure rolls everything back | `cards/secret.rs` `commit`: `Envelope::put` then `saved_in` inside one `claim_tx`; the key store is read BEFORE the transaction | `Commit(d)` (the model's `AtomicCommit`; `Write` + `Finish` are the host's two steps) |
| the write failed (core): the claim is released, the card stays open | `provide` -> `release` | `Fail(d)` |
| the writer dies, and its claim is dropped (dead pid: start time and ESRCH only, as the gate) or runs out (no pid) | `void_dead_claims`; `Card::holder` (a pid claim holds past `claim_until`) | `Crash(d)`, `Lapse` |
| the clock: an open card past its deadline expires | `cards/mod.rs` `sweep` -> `cards_sweep` (`deadline < now`; the claim's pushed deadline keeps it out) | `Tick`, `Sweep` |
| the clock (core) | `cards/mod.rs` `sweep` (`EFFECTIVE < now AND NOT PID_HELD`) | `Tick`, `Sweep` |
| the asker cancelled (core) | `cards/mod.rs` `withdraw` (waits for a held claim) | `Withdraw` |
| the asker's process is gone (core) | `cards/mod.rs` `park_run`: the card stays open, a held claim stays | none (it changes nothing the model tracks) |
| the owner's no (core) | `cards/mod.rs` `answer(allow=false)`: refused while a claim holds; a yes to a secret card is refused (`NotAllowed`), only `saved_in` spends one | `Deny` |
| the owner's no (or a cancel) | `api/bots.rs` `respondPermission` routes a secret card to `secret::respond` before anything else -> `deny`: `Slot::take` (taken: "closed") -> `card_answer(allow=false)` | `Deny` |
| "Another account" | `respond` `"another"`: takes the slot, rewrites the entry into a Key card; closes nothing, writes nothing | none (it only holds the slot for a moment) |
| who may answer | `api/roles.rs` `guard`: a secret card's entry is refused to everyone but the owner; `api/cards.rs` `provideSecret`, `listKeys`, `revokeKey` are owner-only methods | outside the model (the invitee-access skill checks it) |

Closers that cannot reach a secret card, checked by reading: `withdraw_person` / `cards_withdraw_person` (refuse the
owner's cards, and a secret card's person is empty), `withdraw_entry` and `delivered` (only the bot actor's own ACP
permission requests, `self.perms`; a secret card's entry is added by `show`, not by the actor), `card_resume_drop`
(only approved or used cards). `cards_restart`, `card_park`, `cards_park_run` and `cards_park_bot` keep a card open.

## Variants (the switches) and how we know it can fail

| Variant | Switches (`Exclusive`, `ClaimBlocksClose`, `FailWritesNothing`) | What it stands for | Fails |
|---|---|---|---|
| `code` | TRUE TRUE TRUE | the code as built | nothing |
| `no-slot` | FALSE TRUE TRUE | no `Slot`: a second device claims too | `AtMostOneWrite` |
| `no-claim` | TRUE FALSE TRUE | no claim: the sweep and a no close a card mid-write | `ClosedNeverWritten`, `WriteWhileOpen` |
| `fail-writes` | TRUE TRUE FALSE | the assumption dropped: a write the host counts as failed did change the Keychain | `ClosedNeverWritten`, `AtMostOneWrite`, `WrittenEndsSaved` (`WriteWhileOpen` still holds) |
| `core` | all six TRUE | the core as built: single commit, writers can crash, a live writer's claim is never lapsed | nothing |
| `core-split` | `AtomicCommit` FALSE, rest as `core` | the core's order if it wrote the key and then saved the card in two steps (the host's) | `AtMostOneWrite`, `WrittenEndsSaved`: the writer dies between the two steps, its claim lapses, the owner retries and writes twice |
| `core-lapse-live` | `AtomicCommit`, `CrashPossible`, `LapseNeedsDeadWriter` FALSE | M2-11 precondition 1 undone: a claim that runs out by time is taken over under a live writer, in the two-step order | `AtMostOneWrite` |
| `host-old-withdraw` | `WithdrawWaits` FALSE, rest as `code` | the host's withdraw before M2-12, which ignored the claim | `ClosedNeverWritten`, `WriteWhileOpen` |
| `core-atomic-lapse-live` | `LapseNeedsDeadWriter` FALSE, rest as `core` | the same lapse with the single commit | nothing: the zombie's commit finds the claim gone. The pid rule only saves the work; the single commit is what makes it safe |

The switch order is `Exclusive ClaimBlocksClose FailWritesNothing AtomicCommit CrashPossible LapseNeedsDeadWriter WithdrawWaits`; the
first four host variants have the last four at FALSE FALSE TRUE TRUE.

## Bounds and assumptions

- One card, two devices, the card's own deadline as one boolean (`late`: passed or not; the clock only moves
  forward), at most two writes in `fail-writes`. Time is not modeled: `CLAIM_MS` (5 min) outlasts the write's
  `TOOL_TIMEOUT` (20 s), so the model takes "the claim keeps the sweep out" as exact.
- One host process: the slot is in memory. A host restart loses it and kills the `kith-auth` child (`kill_on_drop`);
  restarts are not modeled.
- **A failed write wrote nothing** (`FailWritesNothing = TRUE` in `code`). This is an assumption about kith-auth, not
  something the host guarantees, and the `fail-writes` rows show what it hides. In the code a write can fail after it
  changed the Keychain: `cmd_set_stdin` puts each variable with its own `keychain_put` and dies on the first that
  fails (the earlier ones stay, in a rotation mixed with the old key's other variables), and the host kills a
  `kith-auth` that runs past `TOOL_TIMEOUT`, which may already have saved `auth.json` and the grant. The host then
  releases the claim and leaves the card open, so (TLC's counterexample for `fail-writes` / `ClosedNeverWritten`):
  the owner types a key, `kith-auth set` writes it and is reported failed, the card stays open, the owner says "Not
  now" (or the clock expires it): the card shows denied, the key is in the Keychain and may be granted. A retry is a
  second write. Not a property failure on the code as modeled; a gap to close in kith-auth (all variables or none) or
  by having the host check `keys` after a failed write.
- **The core** (`core*` variants): the writer is the daemon's own thread, so its pid is the claim's pid and the key
  store's child tools (`security`, `secret-tool`) are waited for and killed at their 30 s bound inside the call: no
  writer outlives its claimer. `CLAIM_MS` (5 min) is past that bound several times, which is the second way the review
  allowed (time past the kill timeout) for a caller that gives no pid. The write and the save are one SQLite
  transaction, because `secret_records` and `cards` are the same core.db: a crash leaves neither. What the model cannot
  see: the key store's own state (the master key is read before the transaction, so a first-ever key is minted inside it,
  where a crash can leave a key in the OS store and no records, which the next put adopts), and the Access card ("use
  this account"), which waits for lane B's grants table.
- Not modeled in the core: a `release` that fails after a failed commit (the claim then names the live daemon; the next
  row call frees it, `forget_own_claims`); the key store's child tools are not killed if the daemon is (they are bounded
  at 30 s); no grant is written on save (the host's `kith-auth set --grant`), which waits for lane B's grants table.
- The value itself never leaving `provideSecret` -> stdin is not modeled; `the_key_is_in_the_keychain_call_alone...`
  in `cards/secret/tests.rs` checks it.

## When you change covered code

1. Run `verification/secrets/drift.sh`; read each diff it lists, including new code that closes a card (a new closer
   is a new action), answers a secret card by another route, or writes the Keychain for one.
2. Decide per change: no effect; the model no longer matches (the slot and the claim split apart, a closer that does
   not check the claim, a write before the claim); or the promise changed on purpose (cite it). Update `Secrets.tla`
   and `run.sh` from the code, not from what makes a run pass.
3. Run `run.sh`. A row that differs is drift (replay `--show` against the code; a step the code cannot take means fix
   the model) or a bug (every step can happen: report it with the steps in the code's words and the harm; do not
   weaken the property).
4. A pass on changed code may have lost the ability to fail: break the new check in a scratch edit, see a row or a
   test fail, drop the edit. Add a variant for a new guard.
5. Only then set `SYNC` in `drift.sh` and the sync commit above to the commit you checked.

## Map of the folder

| File | What |
|---|---|
| `verification/secrets/Secrets.tla` | the model; three switches (CONSTANTS) choose the code as built and its variants |
| `verification/secrets/checks.tsv` | variant, kind, property, expected verdict |
| `verification/secrets/run.sh` | runs every row (the switches per variant are in its `switches`); `--show`, `--list` |
| `verification/secrets/drift.sh` | the covered files changed since the sync commit |

## The switched write path (2026-11-12, S-b)

On the authority tombstone `kith-auth set <profile> --stdin-json [--grant AGENT]` (what a card's `provide` runs through
`write_key`) writes the Keychain item first (`keychain_put`, read back), then makes ONE core call, `people.secretSet`
(`core/runtime/src/authority/secret_writes.rs`), which records the reference `keychain:kith/<profile>#VAR` and the grant in
one transaction. The core gets variable NAMES, never a value. Before the Keychain write kith-auth probes `people.revision`:
no daemon, or authority back on auth.json, is exit 3 with nothing stored. The card's claim stays held across the whole call,
so it covers both steps; nothing in `cards/secret.rs` changes.

The model has it as the `switched` variant (`Switched`): `KcWrite`, then `CoreCommit` / `CoreFail` / `CoreLostApplied`
(`CoreLostNotApplied` is `CoreFail`: the read-back finds nothing and kith-auth exits 5), and the invariant `GrantHasItem`
(`ref => kc`). `writes` counts what the core recorded, so an orphan item left by a refusal is not a second write (a retry
reuses the item, `add-generic-password -U`); the value changing under the same reference is the `fail-writes` gap again.
Breaks that must fail: `switched-core-first` (`GrantHasItem`: a crash between leaves a grant with no key),
`switched-lost-as-fail` (`WrittenEndsSaved`: no read-back after a lost answer), `switched-claim-gap` (`AtMostOneWrite`: the
claim let go between the two steps). The host's own save after the write (`Finish`) is not a place kith-auth's process
dies, so `Crash` is only before the core commit in these rows.
