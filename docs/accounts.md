# Accounts

Kith runs agents on a managed set of accounts: any number per provider (Claude subscriptions, Claude API keys, Codex
logins, later others), all equal. The set, the choice of which account a run starts on, and what each account is doing
right now all live in the kernel and work from the terminal (`kith account ...`). The host (`kith serve`) and the apps
are clients of the same commands and JSON.

An account id is `claude` (the default login), `claude:<name>` (a login in its own folder,
`<machine>/accounts/claude/<name>`), `claude-api:<profile>` (an Anthropic key held by kith-auth, handed to Claude Code
through `apiKeyHelper`), `codex`, `codex:<name>`. An id of a harness kith doesn't run yet is listed as `unsupported`.
Logins are made by the harness itself in the account's own folder; kith never reads, copies or prints a token.

## Settings (`<machine>/settings.json`, `lib/kith_settings.py`)

| key | meaning |
| --- | --- |
| `accounts` | ordered list of ids, the set. Absent: every agent runs on its harness's default login |
| `account_policy` | `"fill"` (default) or `"spread"` |
| `account_cross_harness` | `false` (default): the global set serves each agent with the accounts of its own harness family only. `true`: accounts of another harness in the set also serve as fallbacks (Claude agent to Codex and back). `agents.<a>.account_cross_harness` overrides it for one agent; `kith account cross-harness on\|off\|reset [--agent A]` |
| `agents.<a>.accounts` | the agent's own list (a subset and an order); replaces the global set for it and may name any harness. `[]` = nothing listed: with a pin, the pin alone; without, the default login |
| `agents.<a>.pin` | `{"id": ID, "only": bool}`: where the agent's runs start. `only: false` (preferred) falls back to the set, `only: true` never leaves it |

The older shape (`account`, `backups`, `agents.<a>.account`, `agents.<a>.backups`) is read as the keys above and written
back in the new shape by the next write (`kith account migrate` does it now): the old main account + backups become
`accounts` (main first, no repeats), an agent's old account becomes its `pin` and the head of its `accounts`, an agent's
old backups become its `accounts` (an explicit `[]` still means "no fallback"). `account_cross_harness` is turned on only when the
old global `backups` named an account of another harness than the old global main's (default login of Claude when there was
none), because that chain said so; an old global `account` of another family never turns it on. Nothing is lost; an old file nothing has
rewritten still works.

## Selection

`kith account pick AGENT [--json]` (`kith_accounts.pick`) is the one selection. For an agent on harness H:

1. the pin first (and alone when `only`);
2. the accounts of H's family (Claude: `claude`, `claude:*`, `claude-api:*`; Codex: `codex`, `codex:*`) in policy order;
3. the accounts of another harness in set order, only when the agent's own `agents.<a>.accounts` lists them or cross-harness
   fallback is on for it (a Claude agent falls over to Codex only then; adding a Codex account changes nothing for Claude
   agents while it is off).

Without any account of H's family in the list: H's default login. Policy `fill` is the set's order, each account until it
is out. Policy `spread` puts first the account with the most room (the fullest window of a fresh reading; no reading
counts as empty), ties by least recently started. Policies are functions in `kith_accounts.POLICIES`, so a later resource
manager can add one. Accounts that are **out** (until a future time) or **signed out** are skipped and listed; if every
candidate is skipped, the first of the order runs anyway (`all_out`) and the harness says what is wrong.

A plain run (terminal, `kith ask`, a schedule) starts on pick's first candidate (on a Codex account for a Claude agent, it runs
Codex there, as `--account` would). A handed-on session (a channel, `kith ask -c`) stays on the account it was made under while
that is a ready candidate; `--resume` runs under it and notes a start only for it; a one-shot another run or a schedule
started also moves on at a usage limit: it marks the account out (`rate_limit`, an hour unless told) and runs the next.
`--account ID` (`--backup` is the old name) is explicit and wins: no retry, no mark. A launch for a program
(`--launch-json` without `--account`) and a guest's run use the static first account (pin, else the set's first), because
the host picks per turn itself. Every launch notes `last_started`. A launch on `claude:<name>` first syncs that folder with
the main login's settings, plugins and auto-mode flags (`prepare_claude_dir`), so agents stay in auto mode on every account.
A session resumes only under the account that made it.

## State file (`<machine>/accounts/state.json`)

Local to this Mac, owned by the kernel, written by every run and by `kith account out|clear|usage`. Never an account folder
(`list` reads `accounts/<harness>/<name>` only). Writers take an flock on `state.json.lock`, read again under it and swap in
a new file (temp + rename, mode 0600). A missing or corrupt file means nothing is out (a corrupt one is kept as
`state.json.corrupt`). `out_until` in the past means ready. No secret, token or credential ever goes in it.

```json
{"version": 1, "accounts": {"claude:work": {
  "out_until": 1900000000, "reason": "rate_limit", "since": 1899996400, "cleared_at": 1899990000,
  "usage": {"read_at": 1899996000, "windows": [{"name": "five_hour", "percent": 71.5, "resets_at": 1900001000}]},
  "last_started": 1899996400}}}
```

`reason` is a short word (`rate_limit`, `usage_95`, `manual`). A usage reading older than 30 minutes counts for nothing.
The old out files are folded in: `serve/kernel-backups-out.json` is imported once (renamed `.imported`); the host's
`serve/backups.json` is read live, for as long as the host keeps one, and ignored for an account cleared after it was marked, until the host marks it again or extends the mark (`cleared_host_until`).

## JSON contracts

**`kith account list [--agent A] --json`**: an array, one row per account (default logins of installed harnesses, account
folders, every id in a set or pin, bad ids too):

```json
{"id": "claude:work", "harness": "claude", "account": "work", "kind": "subscription",
 "in_set": true, "order": 2, "status": "out", "ready": false, "why": "out of usage", "fix": null,
 "out_until": 1900000000, "reason": "rate_limit",
 "usage": {"read_at": 1899996000, "age_s": 400, "windows": [{"name": "five_hour", "percent": 71.5, "resets_at": 1900001000}]},
 "last_started": 1899996400, "pins": [{"agent": "echo", "only": false}],
 "logged_in": true, "name": "work", "dir": "/Users/me/.kith/accounts/claude/work"}
```

`status` is `ready`, `out` (with `out_until` epoch seconds and `reason`), `signed_out` (`why`, `fix`: the command that makes it
ready), `unsupported` or `invalid`. `kind` is `subscription`, `api-key` or `unknown`; `harness` is `null` for an invalid id.
`in_set`/`order` are the global set's, or with `--agent` that agent's order (pin first); with `--agent` each row also has
`candidate` (its place in pick, or null). `logged_in`, `name` and `dir` are the older fields (`name`/`dir` for folders only).

**`kith account pick AGENT --json`**:

```json
{"version": 1, "agent": "echo", "harness": "claude", "policy": "fill", "pin": {"id": "claude:b", "only": false},
 "cross_harness": false, "candidates": ["claude:b", "claude"], "all_out": false,
 "skipped": [{"id": "claude:a", "status": "out", "why": "out of usage", "out_until": 1900000000, "reason": "usage_95"}]}
```

`candidates` is never empty (all skipped: the first of the order, `all_out` true). `skipped` entries have `status` as above.

**`kith AGENT --accounts-json`** (the host's view): `{"version": 1, "agent", "harness", "policy", "pin", "cross_harness", "main", "order",
"candidates", "all_out", "skipped", "accounts": [row, ...]}`: `order` is the static order (pin, family, then others),
`main` its first, `candidates` what pick returns now, `accounts` a row (as above) for each id in `order`. Readiness is asked of the harnesses (a second or two with several accounts).

**`kith AGENT --backups-json`** (kept for the current host, shape unchanged): `{"version": 1, "agent", "chain": [{"id",
"harness", "account", "kind", "ready", "why"?}]}`, the static order as a chain, entry 0 `kind: "primary"`. No usage or out marks
in it. `kith backup list --json` keeps `{agent, source, backups, main}`.

**Writes for a client**: `kith account set ID... [--agent A]` (the whole list in one write; `set --reset --agent A` drops
the agent's own list), `pin`/`unpin`, `policy`, `out ID --until T --reason R`, `clear ID`, `usage ID --json READING` with
`{"windows": [{"name": "five_hour", "percent": 71.5, "resets_at": 1900001000}], "read_at": 1899996000}` (1 to 8 windows,
percent 0 to 100, nothing else accepted; `read_at` defaults to now).

Older commands still work with the same effect and are deprecated: `kith account use`, `kith backup list|add|remove`,
`kith account backup set|reset`.

## Host (`kith serve`)

The host is a client of everything above; it keeps no set of its own.

**Per turn.** An hq agent's chain is `kith AGENT --accounts-json`: `candidates` (what `pick` would try now) first, then the
rest of `order`; `main` (the plain launch) is the entry marked `primary`. Each turn starts on the first entry that is ready
and not out: the plain launch when that is `main`, `--account ID` otherwise. The same code serves a typed message, an agent
asked by another agent (`ask_bot`, `message_bot`) and a channel's run (a `kith ask` an agent runs itself is the kernel's: pick
and retry, nothing pinned by the host); a chain the host has not read yet for an agent is
read before its first turn, so an account the kernel already has out (marked by a terminal run, or by another agent) is
never started. A turn that ends on a usage limit marks the account out in the host and tells the kernel
(`kith account out ID --until T --reason rate_limit`), then goes on with the next entry once. The chain is read again after
five minutes and after every write below, so a `kith account clear` in a terminal reaches the host within that time; its out
marks replace the host's own (a limit met after the read began is kept until the next read). When every candidate is out,
the host does not run an account that is out: it tells the user which are out and when the first is back (the kernel's own
runs start the first one anyway). A pin with `only` that is out is a chain of one, so it runs, as in the kernel.

With a kith that has no `--accounts-json` the host reads `--backups-json` and keeps its own `backups.json` (the kernel reads
it live) as before. With one that has it, the host writes no `backups.json`; an old file is still honoured by the kernel until
its marks expire or are cleared. The host does not push usage readings (`kith account usage`).

**Methods** (owner only, on the local API and a paired phone; never on a member's or bot guest's list). Times in
milliseconds. Every write takes one lock (the last wins) and answers with the fresh `accounts` read.

| method | body | does |
| --- | --- | --- |
| `accounts` | none | `{policy, crossHarness, accounts: [row], agents: {name: view}}`, see below |
| `accountsSet` | `{agent?, ids}` or `{agent, reset: true}` | `kith account set` (the global set, an agent's own list, or drop the agent's list) |
| `accountPolicy` | `{policy: "fill"\|"spread"}` | `kith account policy` |
| `accountPin` / `accountUnpin` | `{agent, id, only?}` / `{agent}` | `kith account pin|unpin` |
| `accountCrossHarness` | `{agent?, value: "on"\|"off"\|"reset"}` | `kith account cross-harness` (`reset` needs an agent) |
| `accountClear` | `{id}` | `kith account clear` ("it is back"); the host forgets its own mark and reading too |
| `accountAdd` | `{harness: "claude"\|"codex", name, yes?, cols?, rows?}` | a new Claude account first answers `{ok: false, warning}` (the CLI's own text, nothing made); with `yes`, opens a terminal running `kith account add H N --yes` and answers `{account: {ok, id, harness, name, created}, term}` |
| `accountRemove` | `{id}` | `kith account remove ID --yes`; the app confirms first |

`row` is a row of `kith account list --json` in camelCase, without the folder: `id, harness, account, kind, inSet, order,
status, ready, why, fix, outUntil?, reason?, usage: {readAt, windows: [{name, percent, resetsAt?}]}|null, lastStarted, pins,
loggedIn`. `view` per hq agent: `harness, pin, own` (its own list, else `null`), `order, main, candidates, allOut,
crossHarness, skipped: [{id, status, why, outUntil?, reason?}]`. The older `backups`, `backupsSet` and `accountSet` keep
working and read out marks from the kernel's rows as well as the host's.
