---
name: secret-cards
description: Model-checks (TLA+/TLC) that a secret card (a Key or Access card, an agent asking the owner for a service's key) writes the Keychain at most once, only while the card is open, never after a no or the clock closed it, and that a failed write leaves the card open. Use when a change touches serve/host/src/cards/secret.rs (provide, respond, deny, grant, Slot, write_key), the card store's claim and closers in serve/host/src/store/cards.rs (card_claim, card_claim_release, card_saved, card_answer, cards_sweep, card_withdraw), the closers in serve/host/src/cards/mod.rs, provideSecret / listKeys / revokeKey in serve/host/src/api/cards.rs, the secret routing of respondPermission in api/bots.rs or roles.rs, or kith-auth set --stdin-json / grant / keys / revoke --profile. Also use to answer "can a key be saved twice, or after the owner said no?".
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
verification/secrets/run.sh                          # every row of checks.tsv (14 rows, about half a minute)
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

## Sync commit

**b241d7bbd3372fb6bd31bf1024c10b179ef2645b** (origin/main on 2026-10-28; first written at ed2ceb63, after the secret
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
| the clock: an open card past its deadline expires | `cards/mod.rs` `sweep` -> `cards_sweep` (`deadline < now`; the claim's pushed deadline keeps it out) | `Tick`, `Sweep` |
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
