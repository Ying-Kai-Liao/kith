> **2026-11-03: superseded from A4 onward by [runtime-design.md](runtime-design.md)** (agreed by Kai). That design
> replaces this one's section 3, the per-module switch for protocol modules (claims, secrets, scheduler): they go into
> one daemon with the core inside and a holder below, in the order M0-M5. A0-A3 (paths, engines, launch) stand as built.
> The rest of this document is unchanged below.

# Core design: the Rust kernel crate (A0)

Status: **reviewed by the architect agent, awaiting the user's call on Q1 and Q2** (2026-11-03). Part of [one-bot-model.md](one-bot-model.md), track A. Built on the survey
[cli-host-gap-map.md](cli-host-gap-map.md). This is a design, not code: it fixes names, boundaries, order and the
decisions the user still has to make. A1 starts from section 2.

## 1. What the core is, in one paragraph

One Rust crate, `kith-kernel`, built in a new top-level `core/` folder beside `kernel/`, that owns the rules everyone else copies today: where folders are, how an agent is
defined, which engines exist and how to start them, where secrets live, when jobs run. The `kith` binary (already the
host binary) answers the verbs that moved; the host links the same crate. The bash and Python kernel stays for what
has not moved and asks the binary for what has. The CLI's commands and `--json` stay the contract, so apps and the
host's JSON readers never notice a module moving.

Non-goals: rewriting the host, the apps, the cloud or the relay; porting the Python line by line; moving anything
still changing fast (packages, agent settings, people: A7).

## 2. The crate

| Question | Decision |
|---|---|
| Name and place | `kith-kernel` in `core/kernel/` (user decision 2026-11-03: the core lives in `core/`, not a new repo, not under `serve/`). Lib name `kith_kernel`. The A1 branch's crate moves from `serve/host/crates/kernel` to `core/kernel` (same package name, same `paths` module). Fyi decision: `core/kernel/` rather than the crate at `core/` itself, to leave room for siblings. |
| One crate or several | `kith-kernel` (modules) plus, from A2, `kith-acp` (`core/acp/`, async, the ACP client). Split further only when something outside the workspace needs it (as `kith-mem` is mirrored). |
| Workspace shape | See "Workspace shape" below. |
| Edition / MSRV | The workspace's (2024, 1.88). |
| Public shape | Each module exposes a plain Rust API (`fn(&Ctx, Args) -> Result<T>`), a serde type for every `--json` it answers, and a `cli` function that turns argv into the same output the Python printed. The host calls the API; the binary calls `cli`. Neither parses the other's output. |
| `Ctx` | A struct built from env and the filesystem (a long-running host rebuilds it when `.migrated` appears, never caching the state folder for life): the folder rules (A1), the acting person (`KITH_PERSON`), the clock, and handles to fakes in tests (see section 11). Nothing in the crate reads `std::env` outside the code that builds `Ctx`. |

### Workspace shape

Today the workspace root is `serve/host/Cargo.toml` (members `crates/{memory,tts,wire,guest}`), with its `Cargo.lock`
and `target/` there. CI, release and kernel code all name that path: `ci.yml` (`working-directory: serve/host`,
`--manifest-path serve/host/Cargo.toml`, `serve/host/target/...` in about 15 lines, path filters), `release.yml`
(`cd serve/host && cargo build`, `serve/host/target/<triple>/release`), `scripts/package-*.sh`, `voice-samples.yml`,
`kith_paths.py::kith_bin()` (looks in `serve/host/target`), `serve/rust-toolchain.toml`.

| Option | What cargo allows | What moves | Verdict |
|---|---|---|---|
| A. `core/kernel` as an **out-of-tree member** of the existing workspace | A `members` entry may be a path outside the root's directory (`"../../core/kernel"`); the member names its root with `package.workspace = "../../serve/host"` (cargo only auto-finds a root in a parent directory). It shares the root's `Cargo.lock`, `target/` and profiles. The host also depends on it by `path = "../../core/kernel"` | nothing: lock, target dir, CI paths, release, toolchain stay; one `members` line, one dependency line, one `workspace =` key | **Recommended** |
| B. Repo-root `Cargo.toml` workspace | Standard | `Cargo.lock`, `target/` (every path above), the toolchain file (found by directory), every `working-directory`/`--manifest-path` in `ci.yml`/`release.yml`/`voice-samples.yml`, path filters, `package-*.sh`, `kith_bin()`; the cloud and apps share the repo and would sit under a Rust root | Later, once `core/` dominates; a mechanical PR of its own |
| C. `core/` its own workspace, host uses a path dependency | Works, but two lockfiles and two target dirs; a shared dependency can drift apart in the host build | a second CI/release step | No |

I could not run cargo in this review (not installed in the worktree); option A's claims are from cargo's workspace
documentation and the A1 PR must prove them: `cargo build -p kith-kernel --locked` from `serve/host`, and
`cargo metadata` listing the member. If the pinned toolchain refuses an out-of-tree member, fall back to the path
dependency alone (the crate then has its own `[workspace]` stub for its tests) and tell the user.

### Module map, in the order A1..A7 lands

| Order | Module | Replaces | Depends on |
|---|---|---|---|
| A1 | `paths` | `kernel/lib/kith_paths.py` rules, `kernel.rs::machine_dir/home_dir/var`, `hqmem::Layout::from_env` | nothing |
| A2 | `engines` (table, detection, install/sign-in descriptors, `LaunchSpec` to command) in `kith-kernel`; the async ACP client and `run_once` in `kith-acp` | `agent/backends.rs`, `agent/acp.rs`, `agent/auth.rs` data, `kernel/adapters/harness.py` | `paths` |
| A3 | `launch` (prompt assembly, settings, MCP config, `--launch-json`) and `accounts` (`kith_accounts.py`: named logins, backup chain) | the `launch` branch of `kernel/bin/kith`, `kith_accounts.py` | `paths`, `engines`, and from `secrets` the **read-and-take-once half only** (`resolve`, `may`, take-once of a once-grant: the launch runs `kith-auth resolve --take-once`, a write under `auth.lock`). That half lands in A3 together with its `invitee-access` and `run-tokens` checks; A5 then moves the rest. `kith-pkg resolve` output and per-agent settings are read through one reader function per file (Python stays their writer until A7/B1) |
| A4 | `threads` (writer claims, session registry) and `delegation` (hop caps) | `kith-sessions`, `terminal/claim.rs`, `registry.rs`, caps in `kith-ask`, `api/delegations.rs`, `chat/team/requests.rs` | `paths` |
| A5 | `secrets` (all of `kith-auth`: store, grants, profiles, once-secrets **and the people/member rows in `auth.json`**, so `may`, `cut_off` and `person --revoke` stay one implementation) | `kith-auth`, host `market/vault.rs`, `logins.rs`, `passwords.rs`, router/voice keys | `paths` |
| A6 | `sched` (jobs, triggers, ticks, run log) | `kith-sched`, host `routines/` | `paths`, `launch`, `secrets` |
| A7 | `agent`/`settings`, `pkg`, invites and the host's people store as they settle | `kith-pkg`, `kith_settings.py`, host invite/people code | the rest |

`agent` (the model, section 5) is read-only data in A1..A2 and grows writers with B1 and A7. Existing Rust that
already is "the core in disguise" moves in when touched: `hqmem` stays its own crate (`kith-mem`) and is linked, not
absorbed.

### Dependency rules

1. The crate depends on: `serde`, `serde_json`, `anyhow`/`thiserror`, `chrono`, `chrono-tz`, `croner`, `rusqlite`,
   `fs2` (locks), `sha2`, a `Keychain` trait (macOS: the `security` binary), and `kith-mem`. No crypto crate: the one-time vault import lives in the host.
2. Never: `axum`, `tower`, `tokio` (blocking functions; the host wraps them in `spawn_blocking`), `ratatui`/`crossterm`, `reqwest` (network goes through a trait the host or CLI
   supplies, for fakes and for the offline CLI), `kith-wire`, the apps' types.
   `kith-acp` is the one async crate (tokio, no axum): the host's existing ACP client moved, so the host keeps one
   runtime; `run_once` for the CLI owns a small runtime of its own. Interactive `kith <agent>` is the engine's own
   terminal session, not ACP, and does not go through `kith-acp`.
3. The core never calls the host: no `localhost` API, no `host.sock`. Where a kernel script calls the host today
   (`kith-call`, `kith-group`, `kith-keys`, `kith-recall`, `kith-sessions resolve`, the run socket) the core exposes the
   operation and the host registers a callback or handles the event (section 4, "Inversions"). A callback is never called while the core holds one of its locks (`auth.lock`, a claim
   lock): release, call, re-acquire and re-check; otherwise a host that calls back into `secrets` deadlocks. A test
   covers it.
4. The host never reads a kernel-owned file: it calls the crate's API (CI check: a grep list of kernel-owned names,
   section 4).
5. The crate writes only inside folders `Ctx` names; it never writes `serve/config.json` (host-owned).
6. A `cargo tree -p kith-kernel` check in CI fails if a banned crate appears.

## 3. How the CLI sits on it

Rule (user, 2026-11-03; see the plan's "Switching over"): the core runs nothing real until a command is switched, and
a command switches one at a time, once its same-CLI-line comparison (A0b) is green across recent merges.

```
kith <verb>        kernel/bin/kith (bash, still the router)
   |-- verb listed in core-switched.txt AND the binary has the module --> exec <kith-bin> kernel <module> <verb> ...
   |-- anything else ----------------------------------------------------> the existing bash/Python script
```

| Piece | Design |
|---|---|
| The list | `kernel/lib/core-switched.txt`: one line per switched verb, `verb-path<TAB>module` (`paths`, `memory check`, `engines list`). Adding a line is the switch, and the PR that adds it cites the green comparison run. Removing a line is the rollback. Both entries read this one file **from the kernel folder at run time** (the binary finds it with `find_kernel`), never embedded in the binary, so a `git pull` or an older binary cannot make `kith serve ...` and the bash entry disagree. Each line may name the minimum binary version (`threads 0.1.4`); `kith doctor` and `kith update` fix a mismatch |
| Which binary | `kith_paths.py --bin` (`kith_bin()`): KITH_BIN, PATH, the host `kith serve` installed, a dev build; it must answer `kith kernel version` with `{"abi":1,"modules":[...]}`. The router moves a verb only if the binary lists its module. The probe is cached by the binary's mtime so a hook does not pay a process per call |
| Override | `KITH_CORE=python` forces the Python side for every verb; `KITH_CORE=rust` forces the Rust side for every verb whose module the binary has (switched or not). Unset = the list decides. The harness (A0b) uses the two values to run one line both ways; users never set it. A pin per state folder is not offered: production routing is the list, the same for every caller of one kernel version |
| Binary missing or older | The verb runs the Python script, silently for reads. `kith doctor` (and its `--json`) prints `core: python (no kith program)` or `core: rust (modules: ...)` so a Mac on the old core for a release is visible. Exceptions: verbs already Rust-only (`memory check|log|...`) keep today's hard error; **modules that share a protocol file with the host (`threads`, `secrets`, `sched`) are never half-switched**: once such a verb is listed, a missing or older binary is an error with the install hint, because two implementations writing one file is the case the models rule out. A checkout whose list is newer than the installed binary therefore errors with the update hint for those modules only. **The crate implements the existing file protocol exactly, frozen across the switch** (old Python and `claim.rs` know no version field, so a version field cannot protect the old/new pair); `thread-writers` is re-run with Python and Rust as two actors. A version field is added only after both sides know it |
| Python callers | `kith_paths.py` stays; once `paths` is switched it asks `kith kernel paths --json` and a CI test checks both agree on the layout matrix (the A1 "or is checked against it in CI" clause, the first use of the harness) |
| Release copies (PR #579) | The kernel is `cli/<version>/` with `cli/current`; the binary ships in the same version folder, so list and binary always match. `paths` treats the kernel's location as an input only for finding the kernel, never for KITH_HOME (#579 removes that derivation; the Rust code follows the post-#579 rules) |
| Output | Switched verbs print byte-identical JSON where Python printed JSON; text is compared after normalisation (section 11). Intended differences are listed there |
| Host's older kernel folder | A host built with the crate runs its direct calls; if the kernel folder it finds is older than the crate's ABI it falls back to the `kith ...` subprocess path for that module, with the same protocol-file rule as above |


## 4. How the host sits on it

The host links `kith_kernel` and its Rust API replaces subprocess calls to `kith ...` and copies of kernel rules. **The host switches module by module too, and never ahead of the CLI**: it reads the same `core-switched.txt` and calls the crate for a module only when the module's CLI verbs are switched (so for A4..A6 a host and a `kith-sessions` never write one file with different implementations), else it keeps its own code. Rollback is the same list line. A1 and A2's data-only modules switch the host at merge (no shared protocol file).

| Host module | Becomes | When |
|---|---|---|
| `kernel.rs` (`var`, `machine_dir`, `home_dir`, `both`) | `use kith_kernel::paths`; `find_kernel` stays (host-specific: finds the shipped kernel dir) | A1 |
| `hqmem/mod.rs::Layout::from_env` | `Layout::from(&paths)` | A1 |
| `agent/backends.rs`, `agent/auth.rs` data tables | `kith_kernel::engines` (host keeps the setup-terminal glue: `term.rs`, `node.rs`) | A2 |
| `agent/hq.rs` (`launch`, `list`, `info`, `schedule`...) | direct calls instead of `kith ...` subprocesses; the subprocess path remains as the fallback when the host runs against an older kernel folder | A3 and on |
| `terminal/claim.rs`, `registry.rs`, `api/delegations.rs` constants and `parked_in`, `chat/team/requests.rs` caps | `kith_kernel::threads`, `delegation` | A4 |
| `market/vault.rs`, `logins.rs`, `passwords.rs`, `chat/router.rs` key, `rich_card/jev.rs` key read, `cards/secret.rs`, `cards/mod.rs::auth_stamp` | `kith_kernel::secrets` (host keeps card UI logic only) | A5 |
| `routines/` | `kith_kernel::sched` (host keeps webhook listener and event source plumbing, handing events in) | A6 |
| `hub/hq.rs`, `api/backups.rs`, `limits.rs` | typed calls instead of untyped `--json` parsing; `serve/contract/kernel/contract.json` is generated from the crate's serde types (today it covers 4 outputs, the host parses about 15) | as each module lands |

**Stops reading kernel-owned files** (replaced by API calls; each a line in the PR that moves its module):
`live/ask/*.parked` (`delegation::parked`), session claims `live/terminal/<sid>.{json,claim}` (`threads`),
`secrets/auth.json` mtime (`secrets::stamp()`, a change counter), memory/role files read in `kith_info.rs`
(`agent::memory_file`).

**Kernel stops writing host-owned files:** `kith-serve relay` writes `serve/config.json`'s `relay_url`. Replace with a
`kith serve relay` verb that is Rust (host binary) only; the bash `kith-serve` verb calls it. Nothing else in the
kernel writes `serve/`.

**Inversions** (kernel scripts that call the host API): the core exposes a `Host` trait with the few callbacks it needs
(`resolve_thread`, `open_key_card`, `call_person`, `summarise_thread`...). The host implements it over its own
state; the CLI implements it over the local host API when a host is running and with a clear "needs the app" error
otherwise. No module imports the host.

**Two actors on one state folder** (host plus a CLI, two hosts on one disk): files the core owns are written under
`fs2` locks it defines (`auth.lock` today, `sched.lock`, per-thread claim files); the host takes the same locks by
calling the same functions. Rule: one file, one writer module; a second process uses that module, never the file.
This is what the TLA+ models assume (section 8). The protocol files (`live/terminal/*.claim`, `auth.json`, `jobs.json`) stay byte-compatible with today's writers across the switch; both sides use `flock`, as `fs2` does on Unix. Lifecycle intents (`kith-lifecycle` and the host's `terminal/intent.rs` write them) join A4's `threads` module.

## 5. Agent model

One agent = a folder, plus state. Nothing about an agent lives only in a host database.

| Part | Where | Owner module |
|---|---|---|
| Definition ("role") | `<home>/agents/<name>/role/` (a git repo: `agent.md`, skills, `skills.json`, tools) | `agent`, `pkg` (A7) |
| Its life | `<home>/agents/<name>/{memory,data,files}/`, `jobs.json` | `agent`, `sched`, `kith-mem` |
| Per-Mac state | `<state>/` : `settings.json` (permission mode, per-agent model/effort/harness: B1), grants in `secrets/auth.json`, `live/`, `logs/`, cost ledger | `settings`, `secrets`, `threads` |
| Launch spec | computed, never stored: `launch(agent, run options) -> LaunchSpec` (the `--launch-json` contract) | `launch` |

**What the host keeps per bot (look only):** avatar, pin, order, notification preferences, the chat transcript and its
UI rows (the store). It keys them by the kith agent name. A bot row without a kith agent name is the legacy host-native
bot; it is migrated (section 10) and then the column is required (C5).

**Scratch agent (C1):** a built-in agent shipped inside the kernel (`share/kith/builtin/scratch/`, read-only role),
whose life folder is created lazily under the state folder, not under `agents/`, so it is never backed up or shared:
`<state>/scratch/<person>/` with the same `{memory,data,files}` shape, one per person so a guest's quick chats and the owner's never share a memory. Python tools that list agents (`kith ls`, recall, tidy, `kith-ask`) learn `paths::agent_dir` through a small Python shim in A1, or C1 waits for them. Whether an engine can resume a session moved to another folder is unchecked: C1 tests it before promising "keep as agent" keeps the thread. `paths::agent_dir("scratch")` is the one function that
knows the difference; every other module asks it. "Keep as agent" = `kith new` seeded from a thread, then move the
thread's session id. Engine and model are per thread (`launch` options), not per agent.

**Plug-ins from B-track:** B1 adds per-agent `model|effort|harness` to `settings`, read by `launch`; B2 has landed in Python (PRs #589, #590: installed connectors become secret-free kernel services, `kith connector ...`, `kith <agent> connect|disconnect|connectors`, `skill add|remove|skills`, with the host's Marketplace calling them). Its grants are `auth.json` profiles and the agent's skills list, read by `launch` to build MCP
config; the crate takes them over inside `secrets` and `agent` with the CLI contract unchanged, and the harness corpus already has a case over `connectors/skills --json`. Both start in Python and land in the crate under `settings` and `secrets` without changing the CLI contract.

## 6. Folders (A1)

**Spec:** `kernel/lib/kith_paths.py` as written today is the specification; the Rust port reproduces it and a table
test is generated from it (below). Resolution order, restated:

| Result | Rule (first match) |
|---|---|
| state folder | `KITH_STATE` (`HQ_STATE`) if set (a `<home>/state` value with `<home>/kernel` is the old link layout: not "mine") ; `~/.kith/.migrated` exists -> `~/.kith`; `~/.hq` is a real directory -> `~/.hq`; `~/.hq` is a link to `~/.kith` -> `~/.kith`; `~/.hq` links elsewhere -> `~/.hq` (not mine); `~/.kith` exists -> `~/.kith`; else `~/.kith` (not mine until install makes it) |
| home | `KITH_HOME`, else `~/kith` if a dir, else `~/hq` if a dir, else `~/.kith` when it is a link to a dir, else the folder around the kernel (if the kernel sits as `kernel/`) else the kernel folder |
| agents | `<home>/agents` (older: `<state>/agents` as a second root) |
| machine, secrets, logs, cache, store | "mine": `<state>`, `<state>/secrets`, `logs`, `cache`, `store`; else `~/.config/kith` (or `hq`), `~/Library/Logs/kith`, `~/Library/Caches/kith`, `<home>/agents/.store` |
| env | `KITH_X` wins over `HQ_X`; `HQ_BIN` is `KITH_KERNEL_BIN`; export both spellings to children |
| address vs real path | both returned; permission rules use the real one |
| guest rules | `guest_tmp`, `in_guest_folders`, `guest_scrub` move with it (sandbox facts, same module) |

**Known drift, to close in A1:**

| Caller | Drift |
|---|---|
| `kernel.rs::machine_dir` | matches the state rule; `home_dir` ignores the `~/.kith`-link and kernel-folder fallbacks |
| `hqmem::Layout::from_env` | picks `~/.kith` whenever it exists, never reads `.migrated`, and ignores a real `~/.hq`: on a Mac mid-migration (`~/.hq` real, `~/.kith` holding only `serve/`) a bare `kith memory log` reads the wrong folder |
| `cards/mod.rs::auth_stamp` | three candidate paths for `auth.json` |

**Method:** a fixture generator builds ~25 home layouts (fresh, migrated, `~/.hq` real, `~/.hq` link to `~/.kith`,
link elsewhere, `.migrated` with a leftover real `~/.hq`, env overrides, an external-disk `~/kith` link, space in a
path, HQ_ and KITH_ both set and disagreeing) as directory trees in a temp dir. The Python and the Rust each print the
full table for each; they must be equal. New layouts found in the field are added to the generator.

**Findings from the A1 crate branch (`flow/rust-folder-rules-crate`, crate `kith-kernel`, module `paths`, std only,
`kith paths [--json|--sh]`, parity test over 28 layouts; the naming here matches it):**

| # | Finding | Ruling |
|---|---|---|
| 1 | `Layout::from_env` ignored `.migrated` and a real `~/.hq` | fixed in A1; the only behaviour change |
| 2 | Old link layout (`~/.hq` links elsewhere): Python gives state `~/.hq` but machine `~/.config/kith` (not mine); the host's `machine_dir` gives `~/.hq` | Python is the spec: state `~/.hq`, machine things in `~/.config/kith` |
| 3 | Python has no `KITH_AGENTS` override; `Layout::from_env` honours one | keep the override, add it to the spec and to Python |
| 4 | "Folder around the kernel" needs the kernel dir; the host falls back to `~/kith/kernel` when it finds none | `Ctx` takes the kernel dir as an input; the host passes `find_kernel`; the fallback is the Python rule |
| 5 | `//` leading paths and `~user` expansion not matched | out of scope, listed as known gaps |
| 6 | The parity test needs the built binary, so it runs in the host-build CI job | the A0b harness likewise runs where the binary is built; the Python job runs only `KITH_CORE=python` |

**Mid-migration Macs:** `paths` never writes. A running host picks up the flip by rebuilding `Ctx` when `.migrated` appears; `kith migrate` also stops the host first (to be confirmed in A1: not checked here). It reports `{state, mine, migrated: bool, legacy: bool}`; `kith doctor`
and the app show "legacy layout" from it. `kith migrate` stays Python until A7; it writes `.migrated` last, so a fresh
reader flips at one moment.

## 7. Engines over ACP (A2)

**One table**, in Rust, one row per engine; the 27 host rows are the base. The kernel's 8 harnesses are rows of the
same table (the kernel's `agy` is the host's `antigravity`: alias kept in `aliases`). Columns:

| Column | Meaning |
|---|---|
| `id`, `name`, `aliases` | `claude`, `Claude Code`, `["claude-code"]`; `antigravity` aliases `agy` |
| `bins` | program names that mean installed |
| `acp` | how to speak ACP: `local` (the CLI's own ACP mode, `{bin} acp`) or `registry` (adapter entry/pinned build) |
| `install` | per platform: script, npm package, none |
| `login` / `signed_in` | the sign-in command and the cheap probe (args + predicate) |
| `models` | how models are listed (ACP session config or none) |
| `oneshot` | `native` for claude until a test shows ACP reports the same cost as `claude -p --output-format json` (`kith ask` hot path), and for engines with no ACP mode (hermes and dsh are not among the host's 27 and may have none: check in A2); `acp` otherwise |
| `env` | keys the engine reads (feeds `secrets` in A5) |
| `enforces` | what the engine can enforce for kith: tool hooks (PreToolUse deny, PermissionDenied, ledger hooks), permission modes, per-run MCP config, role prompt delivery (flag, env, first message), interactive flags |

* **One launcher:** `engines::spawn(engine, LaunchSpec) -> AcpChild` for sessions and `engines::run_once(...)` which
  opens a session, sends the task, collects the final text and cost, and exits. `harness.py` per-engine flag code goes
  away for engines with an ACP mode; its job (turn a `LaunchSpec` into a CLI's flags) is the ACP client's. Engines without one keep a small native entry in the table's `oneshot: native` command template, still data rather than code.
* **Cost parity:** `run_once` records a row in the cost ledger the same way `harness.py` does (via=, agent, harness).
  This is on the differential list (engines' reported cost fields differ).
* **Detection and install stay in the host for UI** (setup terminal, progress): the table gives the commands, the host
  runs them in its terminal. The CLI has `kith engines [--json]` (list with installed/signed-in) and `kith engines
  install|login <id>` printing the command (or running it with `--run`).
* **Guest runs:** kith's runtime guarantees for a run are Claude Code hooks written into `settings.json` (memory-write deny, ledger, PermissionDenied to card, ask token). Other engines have none of them, so **an engine whose `enforces` lacks the needed hooks is not offered to guest runs** (`launch` refuses, the app hides it); the owner may still run it. `engines` never sees tokens.
* **Interactive runs** (`kith <agent>` in a terminal) are the engine's own TUI, not ACP, so per-engine interactive flags are table data (`enforces.interactive`); `harness.py` goes away only as far as these columns replace it.
* **The Windows case** (host bots were the only kind that ran there): `engines` and `acp` compile on Windows; the
  `launch`/sandbox bits do not until later. Not a goal of A2.

## 8. Secrets (A5)

**Model to keep:** kith-auth's. `auth.json` holds names, profiles (`<service>/<account>`), grants per agent, per-person
rows and times; values live in the Keychain (`security -i` fed on a pipe, hex, no argv/env); once-secrets are Keychain
items `kith/once/<id>` with metadata in `auth.json`. One lock (`auth.lock`) for every write.

**Absorbs the host vault:**

| Host record | Becomes |
|---|---|
| Vault slots (`logins`, connector env/headers, Composio key, 1Password token, voice key, router key) in SQLite, XChaCha20 ciphertext, master key in Keychain `dev.kith.serve.credentials` | Profiles under services: `connector/<id>` (env/headers), `login/<site>`, `composio/default`, `onepassword/default`, `voice/default`, `router/default`; values in the Keychain like every other profile. The SQLite vault is read once by the migration and then dropped |
| Jev key read as `JEV_API_KEY` env then vault slot (`chat/router.rs`) and as `kith-auth get --for serve jev-browser TYPESAFE_API_KEY` (`rich_card/jev.rs`) | One profile, `jev-browser/default`, one function `secrets::get(for, service, var)`; the env override stays as a documented override at the single call site |
| Host-only brute force of grants by app (`credentialStatus` etc.) | Wrappers over `secrets` in the host |

Migration discipline: while the `migrated_vault` marker is absent only the vault is written; the import is keyed by slot and safe to repeat (a crash between profile writes and the marker forks nothing); the host reads the vault master key to import, the one named exception to "the host never opens the Keychain". Spawning `security` on hot paths (the router key per message, `connectorCall`) needs a cache, tied to `secrets::stamp()` so any write invalidates it.

**Keychain access:** on macOS the core always writes and reads through the `/usr/bin/security` binary, as kith-auth does (each item trusts that program; reading through Security.framework from the host binary would make every existing item prompt, and again after each re-signed build). It also keeps the `KITH_SECURITY_BIN` fake meaningful for the harness. Off macOS, `Keychain` is a trait with Secret Service and Windows Credential Manager backends (what the host vault's `keyring` use gives today) and a 0600 file only as a last resort named in `kith doctor`. The vault's ciphertext-in-SQLite design is dropped: values in the OS store, metadata in `auth.json`. The decrypt-and-import code (`chacha20poly1305`) stays in the host, which already holds the master key, so the core carries no crypto it uses once.

**Property checks that must keep passing** (run against the Rust module, with their existing models as the spec;
the model files do not change, the code they describe does):

| Check | Guards | Touches in A5 |
|---|---|---|
| `secret-cards` | a Key/Access card writes the Keychain at most once, only while open, never after no/closed; failed write leaves it open | `provide`, `write_key`, card claim |
| `revoke-access` | a revoked person and their bots reach nothing on any Mac; undo restores exactly | `may`, `cut_off`, `resolve_bot`, `person --revoke` |
| `invitee-access` | an invited person or bot guest gets no more than granted | `resolve`, `member`, `bot`, `access`, `job`, `helper` |
| `run-tokens` | a guest run reads nothing beyond its token's audience | the `get`/`exec` guest branch |
| `kernel/tests/kith-auth-*-check`, `kith-auth-props` | existing script and property tests | all of them run unchanged against `kith kernel secrets` |

**Revoked:** kith-auth stays the only authority for "revoked" (the app's People rows come from the host store; only `revoked` is read from kith-auth). `revoke-access` is run with the host store as a separate actor. **Scratch:** grants, budgets and `may` are keyed by agent name, so a guest's scratch needs a fixed preset (which tools it gets) and an `invitee-access` re-run in C1.

**Rules:** the crate never prints a value; `get` is the only reader and takes `for` (who) and logs to `auth.log`; the
host never opens the Keychain itself (no `security`, no keyring crate in the host).

## 9. Scheduler (A6)

| | `kith-sched` (Python) | `routines/` (host) |
|---|---|---|
| Storage | `<agent>/jobs.json`, state `~/.kith/sched/`, logs | host SQLite rows |
| Time | 5-field cron in **local time**, `@hourly/@daily/@weekly/@monthly`; catch-up: one run if the miss is under a day | 5-field cron with an **IANA time zone** stored per routine; `Interval{seconds}`, `Once{at}`, `Webhook`, `Event{source,event,filters}` |
| Runs as | the job's person, checked against `kith-auth` each time | the bot's owner |
| Driver | launchd `dev.kith.sched` ticks every minute | the host's own clock |

**Proposal: one `Trigger` type, the routines' one, with cron defaulting as follows (decision Q1):**

| Choice | Recommendation |
|---|---|
| Cron dialect | 5 fields as both today, plus the four `@` macros on top. Parsed by `croner` (the crate the host uses); the Python parser's accepted set is a subset, so no existing job changes meaning. `0` and `7` are Sunday in both; to be tested |
| Time zone | Each job stores one. A migrated `kith-sched` job gets `tz: "local"` (follow the Mac, so a travelling laptop keeps today's behaviour); migrated routines keep their IANA zone; new jobs store the zone the caller sends or `local`. Old jobs stay the same and routines keep theirs |
| Intervals, once | Included: `every 30m` sugar for `Interval`; `at <iso>` for `Once`. CLI: `kith schedule add AGENT --cron "..." | --every 30m | --at ISO "task"` |
| Webhook and event triggers | Part of the trigger type; the **fire** path is in the core (`sched::fire(job_id, payload)`), the **listener** (HTTP endpoint, event sources) stays in the host. A CLI-only Mac has no webhooks and says so |
| Driver | A tick (launchd or the host) calls `sched::tick(now)`, the same function either way. The scheduler has three actors on the same state even with one ticker (the launchd tick, `sched::fire` from the host's webhook/event listener, and `kith schedule run` from the CLI), and kith-sched's lock today is per agent (`<agent>.jobs.lock`), so a due-claim per job plus `last_fired` is a new protocol that **gets a TLA+ model before A6 lands**. Moving routines from the host's clock to a one-minute tick coarsens `Interval` and `Once` (an intended difference, listed). `last_fired` lives in machine state (`<state>/sched/`), never in `jobs.json` (that is versioned with the agent, and a commit per run would be noise) |
| Which Mac | `jobs.json` is in the agent folder, which is backed up and synced; routines that ran on one host must not fire on every Mac with the agent. A job gets an optional `mac` field (a stable Mac id, a random id written once to `<state>/mac-id`); absent = every Mac that has the agent, as kith-sched does today; migrated routines get the migrating Mac |
| Who runs a fired job | A subprocess `kith AGENT task` as the job's person, as kith-sched does; the host, when up, shows the run as a thread because the run's outbox already reaches the chat |
| Catch-up | Keep kith-sched's rule (one run if missed by under a day), now applied to routines too |
| Run as | Keep the person rule; routines gain it (owner by default) |
| Storage | `jobs.json` stays the one store (it versions with the agent's memory). Routine rows migrate into the owning agent's `jobs.json` (section 10). Webhook secrets go through `secrets` |

`kith schedule ... --json` gets added to every verb (today none has it).

## 10. Old data and migration

| Data | Where now | Path |
|---|---|---|
| `auth.json`, Keychain items | `<state>/secrets/auth.json`, `kith/*` | unchanged format; the Rust module reads and writes it as is. Any new field is additive and optional (old Python tolerates unknown keys: verified by test before A5 lands) |
| Host vault SQLite | `serve` DB: `vault:v1:` slots | `kith kernel secrets import-host-vault` run by the host on first start of the new version: decrypt each slot with the Keychain master key, write profiles, write a `migrated_vault` marker, keep the old rows for one release, then delete. Failure leaves both stores working (the host falls back to the vault while the marker is absent) |
| `jobs.json` | per agent | unchanged format plus optional `tz`, `trigger`; old files valid |
| Routines rows | host SQLite | export into each agent's `jobs.json` by the host on first start of the new version; the row gets `moved_to`; the scheduler ignores moved rows. Routines for a host-native bot with no kith agent are exported as part of "make it a kith agent" (C5) |
| Host-native bot rows | SQLite `bots` with empty `hq_agent` | C2/C5; this design's only demand is that no core module reads those rows |
| Session claims and `live/` files | `<state>/live/**` | same file names and shapes; A4 implements the protocol the TLA+ models describe; both implementations never write one file: section 3's rule (no half-switching of protocol modules) and the version field |
| `~/.hq` vs `~/.kith` | see section 6 | never migrated by the core |

## 11. Verification: same CLI line, same result (A0b)

Two entry points to the same line, chosen by one switch:

| | Old kernel | New core |
|---|---|---|
| Invocation | `KITH_CORE=python kith ...` | `KITH_CORE=rust kith ...` |
| Meaning | the router ignores `core-switched.txt`; bash/Python runs everything | the router sends every verb whose module the binary has to Rust, switched or not; a verb in a module with no Rust code still runs the old one |
| Default | `KITH_CORE` unset: `core-switched.txt` decides (production) | |

The harness (`kernel/tests/difftest/kith-difftest`, merged in A0b; it already takes `OLD = KITH_CORE=python`, `NEW = KITH_CORE=rust`) takes a corpus line, runs it twice in two fresh sandboxes built
from the same fixture (HOME, KITH_HOME, KITH_STATE, a `fakebin/` first on PATH), and compares:

| Compared | How |
|---|---|
| exit code | equal |
| stdout, stderr | after normalisation: temp paths -> `$TMP`, timestamps -> `$TS`, generated ids -> `$ID`, ordering where the CLI documents none; `--json` compared as data, not bytes |
| state folder | tree diff (names, modes) plus content of text/JSON files, rows of SQLite tables (`memory.db`, others) |
| fakes' call logs | what `gh`, `git` remotes, `security`, `claude`, `codex`, ... were asked, in order (argv with secrets masked) |

Fakes are the ones the kernel tests already use (`KITH_SECURITY_BIN`, fake `claude`, bare git remotes). The corpus
starts from `kernel/tests/*-check` (run them with each `KITH_CORE` value) and the gap map's commands, and grows with every
bug.

**Intended differences** live in `kernel/tests/difftest/intended.json`: line pattern, field/path, new value or rule,
why, PR. An unlisted difference fails. Known entries so far: B0 Claude-model guard; B6 footgun fixes (bare `backup`,
`update`, `migrate` print usage); cost-ledger fields for one-shot runs through ACP (A2); `agy`/`antigravity` alias; the
`paths` `legacy` field (A1, additive); A1 fixing `Layout::from_env` on mid-migration Macs (the old output was wrong).

**Host-side check (A4, A5, A6 replace host code the CLI harness never runs):** the host's own tests run against both
implementations through the `Host` trait fakes, and for A5/A6 a before/after of the same host API call (`listKeys`,
`routines`, `kithSchedule`) on a fixture state. `intended.toml` may not absorb a host-behaviour change without a
named test.

**Not seen by it, covered by the models:** concurrency. The TLA+ specs and property tests (`thread-writers`,
`revoke-access`, `secret-cards`, `run-tokens`, `invitee-access`) run in CI against the new core; each module's PR names
which ones it must re-run (the skills' trigger lists).

**CI triggers:** the model-check jobs fire on paths (`ci.yml`: `kernel/bin/kith-auth`, `serve/host/src/terminal/**`...). Each module's PR adds its `core/kernel/src/<module>/**` paths to those filters and to the matching skill's trigger list in the same PR, or the checks silently stop running. `serve/rust-toolchain.toml` is not found from `core/`; add a `core/rust-toolchain.toml` or run cargo from `serve/host`. Until `rust-rebuild` has CI, the manager runs these checks by hand.

**Switch evidence:** a verb is switched after N=5 consecutive green runs of its corpus on commits merged from main (artifacts linked from the PR that adds the line). `kith kernel version` lists **verbs**, not only modules, so a half-ported module sends only its ported verbs to Rust.

**Per-module exit gate:** corpus green with only listed differences; the relevant property checks green; both
switch values green in CI for one release before the old code is deleted.

## 12. Open questions, with a recommended default

| # | Question | Recommended default |
|---|---|---|
| Q1 | Cron dialect and time zone (section 9) | One trigger type; 5-field + `@` macros; per-job IANA zone with `local` = follow the Mac for migrated jobs. **The user decides before A6.** |
| Q2 | When is the binary mandatory on a Mac (Python copies deleted)? | Two releases after the first module moves, and only after `kith doctor` reports the binary on the owner's Macs; until then fall back |
| Q3 | Crate name/place | `kith-kernel` in `core/kernel` as an out-of-tree member of the `serve/host` workspace (Workspace shape, option A); the paused A1 branch's crate moves there |
| Q4 | Keychain on Linux/Windows | Trait with a 0600 file fallback, flagged by `doctor`; not a goal until a non-Mac release exists |
| Q5 | Scratch agent's folder: under `<state>/scratch/` (never backed up) vs `agents/scratch` with a backup exclusion | `<state>/scratch/` |
| Q6 | Does the CLI-only Mac get webhook triggers? | No; say so in `kith schedule` output |
| Q7 | Who writes the cost ledger row for one-shot runs | `kith-cost record` stays the only writer (as `harness.py` does today: a subprocess); `cost` becomes a module later. `run_once` returns the engine's reported cost and the caller records it |
| Q9 | Must A3 wait for A5? | No, but A3 carries `secrets::resolve/may/take-once` (review point 1); if that proves too big, swap A3 and A5 and drop the 50 ms target until then |
| Q8 | `kith kernel` as the subcommand namespace vs flat verbs | Flat verbs for users (unchanged), `kith kernel <module> <verb>` as the internal route the router execs |

## 13. Review

Architect agent (`kith architect`, answer only), one review of this doc, run on the version that already had the `core/`
and routing sections. Twelve points; each is adopted (A: the change is in the named section) or answered (R).

| # | Point | Ruling |
|---|---|---|
| 1 | A3 reads `kith-auth resolve --take-once`, a write under `auth.lock`; "one reader per file" fails (second copy of the rules, or a Python shell-out that loses 50 ms) | A: `resolve`, `may`, take-once move into A3 with their checks (module map, Q9). A5 moves the rest |
| 2 | Other ACP engines enforce none of kith's hooks; guest runs would silently lose guarantees; interactive runs need per-engine flags | A: `enforces` column; engines lacking hooks are not offered to guests; interactive flags are data (section 7) |
| 3 | The switch list and the binary disagree on checkouts; an embedded list contradicts "remove a line to roll back" | A: one list read from the kernel folder at run time, optional minimum binary version per line, error plus update hint for protocol modules (section 3) |
| 4 | A version field does not guard old plus new; lifecycle intents have two writers and no module | A: the protocol is frozen byte-compatible across the switch, `thread-writers` re-run with two actors, intents join `threads` (sections 3, 4) |
| 5 | The host has no switch, rollback or gate | A: the host reads the same list and switches a module only after its CLI verbs (section 4) |
| 6 | Moving code into `core/` turns the model-check CI off; toolchain file not found from `core/` | A: each module PR adds its `core/` paths to the CI filters and skill triggers; `core/rust-toolchain.toml` (section 11) |
| 7 | Scheduler has three actors, per-agent locks, tick coarsens intervals, `mac` needs an id | A: TLA+ model before A6; `mac-id`; coarsening listed as intended (section 9) |
| 8 | Vault migration needs one owner, a repeatable import, a named host Keychain exception and a hot-path cache | A: migration discipline paragraph (section 8) |
| 9 | "Green across recent merges" has no number; per-module probe vs per-verb switch | A: N=5 consecutive green main-merged runs linked from the PR; the probe lists verbs (section 11) |
| 10 | Scratch shares one identity across people in `auth.json` | A: fixed preset and an `invitee-access` re-run in C1 (section 8) |
| 11 | Cost ledger would get a second writer | A: `kith-cost record` stays the only writer; `cost` module later (Q7) |
| 12 | A5 moves half of "who is revoked"; contract generation; chacha crate; wrong cross-references | A: kith-auth the only authority for revoked, `revoke-access` with the host store as an actor (section 8); contract generated from serde types (section 4); the crypto stays in the host; references fixed |

From the earlier, cut-off first pass of the same review (same doc before the routing sections): A5 and A7 split one
file (adopted: A5 takes all of `kith-auth`); mixed cores on one Mac (adopted: no half-switching of protocol modules);
Keychain prompts (adopted: always `security`); ACP in a blocking core (adopted: async `kith-acp`); scheduler race and
`last_fired` placement (adopted); harness blind to host code (adopted: host-side check); `Ctx` caching across
`kith migrate` (adopted: rebuild on `.migrated`); doctor shows which core runs (adopted); host callbacks under a lock
(adopted: rule 3).

Not settled by review, for the user: Q1 (cron dialect and zone) and Q2 (when the binary becomes mandatory).
