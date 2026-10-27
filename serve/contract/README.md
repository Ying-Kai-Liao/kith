# Contracts

Each word that crosses a boundary is defined once here, and every side takes its types from that definition. No side
spells the word by hand.

| Boundary | Contract | Producer | Consumers |
| --- | --- | --- | --- |
| Cloud HTTP API | `cloud/contract.json` | `serve/cloud` | desktop + web, iOS (KithKit), host (`remote/`), guest crate |

`node serve/contract/gen.mjs` writes each side's types from the contract. It is plain Node with no dependencies. CI
runs it with `--check`, which fails when a generated file differs from what the contract makes:

| File | Used by |
| --- | --- |
| `serve/cloud/src/contract.gen.ts` | the cloud: route table, handler answer types, error codes, a strict check of every answer in tests |
| `apps/desktop/src/shared/cloud-contract.gen.ts` | desktop main + renderer and web: types, `paths`, `receive`, `decodeX` |
| `apps/ios/Kit/Sources/KithKit/Models/CloudContract.gen.swift` | iOS: `Codable` types, `CloudPath`, `CloudErrorCode` |
| `serve/host/crates/wire/src/cloud.rs` | host and guest: serde types, `path::`, `ErrorCode` |

## What holds

- **W1: each word is defined once.** Enum values, field names, paths and error codes live in `contract.json` and nowhere
  else.
- **W2: a change is seen on every side.** Each generated file carries the contract's sha256 as `CONTRACT_SHA256`. Each
  side's tests hash `contract.json` and fail when the generated file is stale. A renamed or removed value is then a
  compile error wherever it was used. In tests, the cloud checks every JSON answer against its route's type
  (`KITH_CONTRACT_STRICT`), so a handler can't send a field or value the contract doesn't name.
- **W3: an unknown value is handled explicitly.** Each enum says in `contract.json` what a value it doesn't know means
  for a consumer (`"unknown"`), and every side's decoder reads it that way. It never decodes to an error.
  - When `"unknown"` names one of the enum's values, that is the least one, and a word this version doesn't know reads
    as it: a role reads as `member`, a computer's access as `none`. The generated enum has no unknown case.
  - When `"unknown"` is `"unknown"`, the word is a state of its own (TS `'unknown'`, Swift `.unknown`, Rust `Unknown`),
    which each consumer's switch must handle. An access request in a state an app doesn't know is over, never approved.
  - `unknown` is a consumer's word only. The cloud never sends it: no enum may list it, and the producer check refuses
    it.
- **W4: older apps keep working.** `fixtures/compat/` holds the answers of an older cloud, and `fixtures/unknown/` holds
  those of a newer one. Every consumer's tests decode both. Consumers ignore fields they don't know (no
  `deny_unknown_fields`; Swift `Codable` and TS `receive` pass them by); only the producer check refuses them.

## Field specs

`"string"`, `"int"` (ms timestamps are ints), `"bool"`, an enum or type name, `"[T]"` for a list. A trailing `?` means a
consumer must accept the field being absent or null. The producer still sends the key, with `null` when there is no
value, unless the type lists the field in `mayOmit`.

An enum is `{ "values": [...], "unknown": "<one of values>" | "unknown" }`, plus an optional `names` for a value a
language can't use as a name (Swift's `none`).

## Changing the contract

Changes are additive only.

1. To add a field, add it with `?`: older clouds don't send it.
2. To add an enum value, add it to `values`. Older apps read it as the enum's `unknown`, so first check that this is
   safe for the new value. Never change an enum's `unknown`: apps already shipped read new values with the old one.
3. To add a route or error code, add it.
4. Run `node serve/contract/gen.mjs`, add a fixture for the new shape, and fix whatever each side's build or tests now
   report.

Never rename or remove a field, value, route or code. Apps already shipped still send and expect the old word. A
removal needs a new route version (`v2/...`) that runs alongside the old one until no supported app uses it.

Fixtures are `{ "type": "<contract type>", "body": <answer> }`. Those in `fixtures/` are what the cloud sends today and
pass the producer check. Those in `compat/` and `unknown/` only need to pass the consumer check.
