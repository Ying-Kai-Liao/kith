# The engine layer: every Kith feature, whatever the harness

Status: design, for Kai to read. No code in this PR. Written 2026-11-27 against `origin/main` at `38e7bc465`
(file:line below are at that sha; lines that move when an open PR lands are marked **moves**).

Kai, 2026-11-26: "I think the proxy layer should be able to work with multiple harnesses." Asked which layer, he chose the
**engine/adapter layer**: the code that starts and talks to an agent's harness (Claude Code, Codex, an ACP adapter, Hermes),
so that wake, ask, schedules, cards and cost work the same through it, whatever the harness.

Decisions already made, not reopened here: Hermes runs outside bypass are refused (#1210). Kith is CLI-first: the
`core/kernel` crate is the core and the host links it instead of keeping its own copy. Files stay small: a code phase that
touches a file over about 1,500 lines splits it first. The architect reviews this design before any worker codes it (section 5).

## 0. What exists, and the one-sentence diagnosis

`core/kernel/src/engines.rs` is already the engine table: 28 rows, an `Enforces` struct of yes/no capabilities
(`engines.rs:205`), `session_modes` (`:133`), `refuses_unbypassed` (`:160`), `allows_granted_mcp_asks` (`:175`),
`serves_guests` (`:255`), `launch_spec` (`:332`). `engines/tui.rs:32` builds the Codex and Hermes terminal argv. Non-Claude
one-shots run in `serve/host/src/engine_run.rs:278` over ACP (`core/acp`).

The table says what an engine **is**. Nothing says what a feature **may do on it**, so every feature decides alone, by
matching on the engine's name. The same question, "is this Claude?", is answered in at least eleven places (section 1), each
with its own wording of the refusal, and the PRs in flight copy more: `harness_of_agent` exists twice (`wake_clock.rs:132`,
`core_ask.rs:318`, the latter with a `TODO(WK4c, #1221)` to dedupe), and S2 #1220 copies `launch_program`'s choice
(`sched_launch.rs:35`, per #1222's body). Each new feature (a wake, a bg ask, a network rule) is built for Claude first and then patched for the
rest. That is the cost this design removes.

## 1. Inventory: where Kith treats a harness specially

Columns: **C** Claude Code, **X** Codex, **A** other ACP engines (the 25 other table rows, one-shot and chat over ACP),
**H** Hermes. yes / partial / no / refused.

Run kinds in the host today: terminal (`kith AGENT`, interactive), one-shot (`kith AGENT -- task`, also `kith ask`, a
bg ask and a schedule), host chat (the app's thread, one ACP process per chat), channel/bot turn, wake turn.

### 1.1 Start, resume, model

| Feature | Where (origin/main) | C | X | A | H |
|---|---|---|---|---|---|
| Choose the path by name | `kernel/bin/kith:1530` (`harness != claude` hands over to `kernel engine-run`), `core/kernel/src/launch/exec.rs:497` (`!plan.claude`) and `:588` `other_engine`, `plan.rs:1100` (`claude = harness == "claude"`) | native argv | `engine-run` | `engine-run` | `engine-run` |
| One-shot argv | Claude: `bin/kith:1582-1713`, Rust `launch/start.rs:59` `claude()`. Others: `engine_run.rs:278` `run()` builds an ACP `RunSpec` | yes | yes (ACP) | yes if `oneshot: Acp` and an adapter is found; exit 78 `NOT_AVAILABLE` for registry-only engines with no Kith-set modes (`runonce.rs:43-55`) | yes, bypass only |
| Terminal session | Claude: `launch/terminal.rs:223`. Others: `terminal.rs:278` `adapter()` then `engine_run.rs:226` `interactive()` then `engines/tui.rs:32` | yes | yes (`tui.rs:73`) | no ("no terminal session for X; give a task", `engine_run.rs:237`) | yes (`tui.rs:146`) |
| Host chat start | `plan.rs:1079` (and the diagnostic verb `launch_cmd.rs:145`, which is not a chat start): `--launch-json covers claude and codex only for now`; other engines only when the table row has an ACP mode (`plan.rs:1079-1087`) | yes | yes | partial (ACP row needed) | yes, bypass only |
| Host's own adapter switch | `agent/bot/launch.rs:169` `adapter()` = Claude / Codex / Other by `initialize` fields; `bot/mod.rs:335` instructions; `hqmem/oneshot.rs:101` | `_meta` | by agent name suffix | generic | generic |
| The wire's own harness enum | `agent/hq.rs:97,235` (`Harness::Claude`/`Codex`/`Unknown`), `other_engine()` `:240` | yes | yes | via `engine` string | via `engine` string |
| Resume, terminal | `plan.rs:994`, `bin/kith:842`: `--resume continues Claude sessions` | yes | no, refused by name | no | no |
| Resume, one-shot / handed on | Claude `bin/kith:1586` (an id containing `:` starts new, with a note). Others `engine_run.rs:134` `resume_id` (`<engine>:<id>` only; a foreign one starts new, with a note) | yes | yes | yes if ACP `session/load` | yes; a failed `session/load` starts a new session (#1210) |
| Session id shapes | bare uuid (Claude); `codex:<id>`, `<engine>:<id>` (one-shot rows, `engine_run.rs:68`); `acp:<chat>:<thread>` (host chat, `bot/hq.rs:397`); `owner/repo#n` (channel thread, `wake_clock.rs:115` `old_door`) | | | | |
| Model name | the Claude-model guard: `plan.rs:958-970`, `bin/kith:831-835` (a Claude name is dropped with a note on any other engine) | yes | by prefix list | by prefix list | by prefix list |
| Model list | `engines.rs` `Models::Acp` / `None`; ACP session config options | `Acp` | `Acp` | per row | per row |
| Effort | Claude `--effort` (`bin/kith:1583`); Codex `model_reasoning_effort` (`tui.rs:80`) and `RunSpec.effort` (`engine_run.rs:307`, `max` becomes `xhigh`); Hermes `--reasoning` (`tui.rs:160`) | yes | yes | ACP only if offered | yes |
| Role prompt | `enforces.role_prompt` (`engines.rs:205`): Flag (C), Flag/Env/FirstMessage per row; Hermes env (`tui.rs:169`); one-shot `instructions_first` (`engine_run.rs:126`) | flag | flag | per row | env |
| MCP servers per run | `enforces.per_run_mcp`; Codex `-c mcp_servers.*` (`tui.rs:92-121`); Hermes: none, said on stderr (`tui.rs:148-153`) | yes | yes | per row | no, noted |

### 1.2 Permissions, cards, network

| Feature | Where | C | X | A | H |
|---|---|---|---|---|---|
| Permission mode | `enforces.permission_modes`; `session_modes` (`engines.rs:133`) lists Codex and Hermes modes; `bot/hq.rs:42` `codex_reach` maps Claude's words to Codex's; `bot/hq.rs:297,308,329` three `set_*_mode` functions | yes | yes (modes by id, two spellings tried) | no: session left in its own mode (`session_modes` returns empty, `engines.rs:149`) | only `dont_ask` (bypass) |
| Hold a run to its folders | Claude: settings plus guest sandbox (`launch/settings.rs:1593`); Codex: `sandbox_mode=workspace-write` + writable roots (`tui.rs:88-96`) or ACP mode; others: nothing | yes | yes | no | **refused** without bypass: `engines.rs:155,160`; `runonce.rs:91` `cannot_hold` (exit 79); `bot/hq.rs:63` `start_refusal` |
| Network inside the sandbox | #1229 (open, **moves** `tui.rs:88`, `exec.rs:604`, `engine_run.rs:232`): owner's Codex terminal sandbox gets `network_access=true` via `KITH_RUN_OWNER` | n/a (owner has no OS sandbox; a guest's sandbox allows only the host socket, `settings.rs:1593`) | terminal only; the ACP one-shot and the chat set no network rule | no | n/a (bypass) |
| Tool cards | ACP `session/request_permission`: `bot/updates.rs:101` (host chat), `core/acp/src/run_once.rs:521` (one-shot: approve, or ask-and-refuse) | yes | yes | yes | yes (never reached outside bypass) |
| Claude-specific card parts | `updates.rs:279` `_meta.claudeCode.toolName`; `updates.rs:701-764` Claude's option ids and "always" | yes | no tool name: guest grants match by `kind` only (`access::acp_in_grants`) | same | same |
| Granted MCP tool asks in a one-shot | `engines.rs:175`: only Codex; every other engine's ask stays refused | n/a | yes | no | no |
| Guests (invited person, bot guest) | `engines.rs:255` `serves_guests` (hooks + modes + per-run MCP): Claude only; `agent/hq.rs:199`, `plan.rs:1090`, `launch_cmd.rs:148` | yes | **no** | no | no |
| Tool hooks (deny, ledger) | `enforces.tool_hooks`; written into Claude's settings (`settings.rs`) | yes | no | no | no |

### 1.3 Cost, usage, accounts

| Feature | Where | C | X | A | H |
|---|---|---|---|---|---|
| Cost ledger writer | Claude: `SessionStart` hook `kernel cost tag` (`settings.rs:799`) then `sync` copies Claude's transcripts (`cost/ledger.rs:249`). Others: `engine_run.rs:208` `record()` writes one row per one-shot, from what the engine reported | hook + transcript | one-shot row | one-shot row | one-shot row; the terminal session: none |
| Cost for a host chat turn on another engine | no writer found at origin/main (`kernel::cost::run` is called only from `engine_run.rs:213` and `main.rs:1084`) | tag hook | none found | none found | none found |
| Usage windows (limit meters) | `usage.rs:108` reads Claude's statusline/CLI and Codex's `sessions`; any other id: `None` | yes | yes | no | no |
| Usage-limit detection | Claude chat: `updates.rs:199` `_claude/rateLimit`; ACP one-shot: `core/acp/src/client.rs:132` `depleted()` on RPC error kinds; CLI chain: `accounts/chain.rs:14` text regex on a failed run's output | yes | via RPC kind | via RPC kind, if the adapter sets it | no known signal |
| Account walk (`KITH_BACKUP_CHAIN`) | `bin/kith:671-677`, Rust `accounts/chain.rs:105`; the chain is only claude and codex: `agent/backups.rs:70` `runnable()`, `accounts/logins.rs:48`, `listing.rs:48` | yes | yes (`CODEX_HOME`) | no | no |
| Session is resumed only under the account that made it | `bin/kith:785-820`, `plan.rs` equivalent; default account name `claude`/`codex` in `agent/hq.rs:248` | yes | yes | no ledger account | no |
| Backup of a Claude agent on Codex | `plan.rs:865-913`, `958`: model dropped, harness switched | n/a | yes | no | no |

### 1.4 Claims, wake, schedules, asks, channels, lifecycle

| Feature | Where | C | X | A | H |
|---|---|---|---|---|---|
| Writer claim + conversation lock | Claude: `launch/start.rs:59` `claude()` takes the claim, then runs under `kith-stub` (`bin/kith:1635-1713`, the lock). Others: no stub, no lock; the claim is attached by `attach_env_claim` (`launch/sched_turn.rs`, called at `exec.rs:497`) | claim + lock | claim only | claim only | claim only |
| `KITH_TURN_CLAIM` / `KITH_TURN_CONVERSATION` (C3L #1228, merged) | `bin/kith:114-122` (Claude), `launch/exec.rs` and `sched_turn.rs` (others) | yes | yes | yes | yes |
| Claim's engine column | `terminal/core_claims.rs:235,330`: `"engine": "claude"` written for host claims whatever ran; `core/runtime/src/conversations/external.rs:336` defaults to `claude` | right | wrong label | wrong label | wrong label |
| Terminal take-over (`--held`, `--resume`, `-c`) | `bin/kith:1530-1532`, `exec.rs:503`: "--held holds claude sessions only" | yes | no | no | no |
| Wake (a held request resumes a thread) | `wake_clock.rs:41,126` `ENGINE_GAP`: a wake turn on a non-Claude agent is cancelled ("this agent's engine cannot run detached turns yet"); `kith-wake:335` `claude_session` | yes | cancelled | cancelled | cancelled |
| Wake via the core's turn (WK4c #1221, open, **moves** `wake_clock.rs`, `kith` launch) | channel/bot doors begin the core's wake turn by id; the launch path is engine-agnostic once `KITH_TURN_CONVERSATION` is set | yes | not yet routed (`ENGINE_GAP` stays) | no | no |
| Schedule fire (S2 #1220, open, **moves** `routines/core_cron.rs`, `launch/sched_turn.rs`) | `kernel launch AGENT --exec --turn N --claim C`: the launch is the same for every engine (`sched_turn.rs:120,191` carries `KITH_HARNESS`); but `bin/kith` is bypassed when the program lacks `sched-turn`, and Windows keeps `kith-sched run-turn` | yes | yes (via `other_engine`) | yes | refused without bypass (79) |
| Foreground ask | `core/kernel/src/ask/fg.rs`; `kith-ask:755`: `--to` needs "a session ... on a harness other than claude" refused | yes | one-shot only | one-shot only | bypass only |
| Background ask (A2 #1222, merged) | `routines/core_ask.rs:81,89` `engine_of`/`is_claude`: Claude callee = `kith AGENT -- task` with `KITH_CHAIN_CHILD=1` (single attempt, **no account walk**); others = `kernel::launch_program()` + `kernel launch --exec` (`kernel.rs:259`) | single attempt | launch | launch | launch, 79 outside bypass |
| Channels and bots | `channels/run.rs`, `agent/bot/*`: same ACP session path as host chat; `bot/launch.rs:169` adapter | yes | yes | partial | bypass only |
| Compaction | `bot/compaction.rs:17`: the engine's own report if it sends one (`reports_compaction`, set on a Claude `compaction` update), else a context-use drop | reported | inferred | inferred | inferred |
| Where "is this Claude?" is re-answered | `core_ask.rs:89`, `wake_clock.rs:126`, `plan.rs:958,994,1079,1100`, `bin/kith` (13 sites above), `launch_cmd.rs:110,127,145`, `backups.rs:70`, `usage.rs:108`, `logins.rs:48`, `core_claims.rs:235` | | | | |

Reading the table: **Claude is the full citizen, Codex is second class everywhere but one-shots, other ACP engines get a
one-shot and a chat, and nothing else, and Hermes gets the same minus anything outside bypass.** The gaps are not
principled limits of the harnesses. Wake, schedules, claims and cards are engine-agnostic by construction (a claim, an env, a
launch). They are Claude-only because the check at the door matches the name.

## 2. One engine interface

### 2.1 Where it lives

`core/kernel/src/engines/` (the existing module, split by concern). The CLI links it, and the host links the same crate
(`serve/host` already depends on `kith_kernel::engines`, `agent/backends.rs:20`). No second copy in the host, no copy in
Python. `kernel/bin/kith` stops matching names the moment its body is the Rust launch (P1-12 R5); until then it asks the
binary (`kith kernel engines launch <id> --json`, already used at `bin/kith:886`), and that JSON gains the new fields.

### 2.2 Shape: a capability table plus three drivers, not a trait per engine

27 rows differ in data; only three differ in behaviour. So:

1. **Capabilities** (data, in the table): replace `Enforces`'s bare bools with a `Caps` value per feature, where each
   feature is a `Support`:

   ```rust
   pub enum Support {
       Yes,
       Partial(&'static str),   // works, with this limit; shown to the person
       No(&'static str),        // cannot; the reason a refusal quotes
       Refused(&'static str),   // could, but Kith decides not to (Hermes outside bypass)
   }
   pub enum Feature {
       Resume, ModelChoice, Effort, PermissionModes, FolderFence, Network, ToolHooks,
       PerRunMcp, Guests, Terminal, DetachedTurn, UsageSignal, AccountChain, CostRow, ClaimLock,
   }
   impl Engine { pub fn can(&self, f: Feature, run: &RunShape) -> Support; }
   ```

   `RunShape` is what the answer can depend on: `{ owner: bool, bypass: bool, platform, kind: Terminal | OneShot | Chat |
   Wake | Sched | Ask | Channel }`. **Answers are keyed by kind from the start**: a Claude host chat runs over ACP
   (`claude-acp`, `_meta.claudeCode`), not natively, and Hermes' row says `per_run_mcp: true` and `FirstMessage` while its
   terminal takes no MCP and gets the prompt by env (`tui.rs:148-169`); a flat per-engine flag would lock in answers that are
   already wrong for some kinds. The same `RunShape` is passed on to engine-run as env (#1229's `KITH_RUN_OWNER` is the first
   field); every launch path sets every field, none inherits it (the bash path does not set `KITH_RUN_OWNER`, `bin/kith:1537`).
   Which features each kind asks `check` for is one table in the crate, not each caller's list. Gating is in two stages: a
   static check before spawning, and a run-time check after ACP `initialize` (`session/load`, model options).
   `can` takes the platform because the launch is Unix-only today (Windows answers No until its port). Hermes' answer to `FolderFence` is `Refused(..)` unless `bypass`; Codex's to `Network` is
   `Yes` for an owner in a sandbox. The existing `refuses_unbypassed` and `HERMES_REFUSES_WITHOUT_BYPASS` become one
   row's answer; the switch (Kai's) stays a single constant.

2. **Drivers** (code, three of them), a closed `enum Driver` with a `match` (not a trait object: the set is closed), chosen from the row *and* the kind of run: `Native` (Claude Code: its own argv, hooks,
   transcripts), `Acp` (the generic `kith_acp` path; every row with `oneshot: Acp`), and a `Tui` add-on for the rows that
   have a terminal session (Codex, Hermes). The pure parts live in `core/kernel`; the async parts (usage reads, ACP update parsing) stay in the host and call the pure ones. The run path uses these functions per driver:

   ```rust
   impl Driver {   // each a match over the three variants
       fn start(&self, e: &Engine, run: &RunInputs) -> Start;   // argv+env, or an ACP command + RunSpec
       fn resume(&self, e: &Engine, s: &SessionRef) -> Resume;  // Continue(id) | New{note}
       fn limit(&self, signal: &Signal) -> Option<Depleted>;    // usage-limit detection for this engine
       fn cost(&self, out: &Outcome) -> CostSource;             // Hook | Reported(row) | None
   }
   ```

   `Start`, `Resume` and the argv builders exist today as free functions (`tui_argv`, `resume_id`, `session_modes`, the
   `RunSpec` build in `engine_run.rs:278`); the trait gathers them behind the table row instead of three `if harness ==`
   branches. Per-engine quirks (Codex's two mode spellings, Hermes' modes) stay in the row's data, as `session_modes` has
   them.

3. **A single gate** that every start path calls first:

   ```rust
   pub fn check(e: &Engine, f: &[Feature], run: &RunShape) -> Result<Notes, Refusal>;
   ```

   `Refusal` carries the engine name, the feature, the reason and the context that picks the exit code (a guest refusal is 3, or 4 under a backup walk, `plan.rs:1096`; the launch-json refusal 1 or 4), and renders the one sentence the person sees
   (`"hermes can't be held to its folders unless the agent is in bypass mode ..."`, today `engines::refusal`). `Notes` are the
   `Partial` limits to print on stderr ("hermes takes no MCP servers per run"). Exit codes map from `Refusal` once
   (`CANNOT_HOLD` 79, `NOT_AVAILABLE` 78, `serves_guests` 3).

### 2.3 How a caller asks

A caller never matches an engine id. It asks a feature:

| Today | After |
|---|---|
| `is_claude(engine)` (`core_ask.rs:89`) | `e.can(Feature::ClaimLock, shape)` and the driver, or just `launch` (the launch decides) |
| `engine_gap` (`wake_clock.rs:126`) | `e.can(Feature::DetachedTurn, shape)`; `Yes` for every engine the launch can start |
| `harness != "claude" && != "codex"` (`plan.rs:1079`, `launch_cmd.rs:145`) | `e.can(Feature::Terminal/Chat)`; the table's row, not a list |
| `runnable()` `claude \| codex` (`backups.rs:70`) | `e.can(Feature::AccountChain)` |
| the Claude model prefix list (`plan.rs:958`) | `engines::model_for(e, requested) -> Use(m) \| Default{note}` |
| `--resume continues Claude sessions` (`plan.rs:994`) | `e.can(Feature::Resume, shape)` and `SessionRef` |
| `serves_guests()` | `e.can(Feature::Guests, shape)` (same answer, same table) |

Which engine runs a turn is decided once, in `engines::for_run(requested, session_ref, agent_harness) -> &Engine`: a resume
runs on the engine that made the session; otherwise the requested engine (`-H`, the turn's); otherwise the agent's own
(`harness:` of agent.md, unset is claude, `claude-code` an alias). Today the doors disagree: the wake door lets the agent's
harness win (`wake_clock.rs:125`), the bg ask door lets a non-Claude turn engine win (`core_ask.rs:81-87`), and tests pin
both. The plan's backup walk can change the engine after a door checked it (`plan.rs:865-913`), so `check` runs inside the
plan once the engine is final, on every attempt; the doors only pre-check. Further copies of the rule: `launch_cmd.rs:108`,
`plan.rs:798`, `bin/kith:294,767`, and the caller `core_runs.rs:167`.

### 2.4 The eight things the brief names

- **Start.** `Driver::start`. One entry for the terminal, one-shot, chat, wake, schedule, ask: the launch builds a
  `RunInputs` plus `RunShape` and the driver turns it into an argv or an ACP spec. What exists in `tui.rs` and `engine_run.rs`
  moves behind it, unchanged byte for byte (the argv is golden-tested, `engines/tui/tests.rs`).
- **Resume.** A `SessionRef { engine, id, key }` parsed from the three stored shapes: bare uuid is `claude`, `<engine>:<id>`
  is that engine, `acp:<chat>:<thread>` and `owner/repo#n` are host conversation keys. **Old data keeps working because the
  parser reads exactly today's shapes and writes the same ones**: no migration. Resume support is `Yes` (Claude, Codex),
  `Partial("if the adapter offers session/load")` for other ACP rows, found at run time from the adapter's
  `agentCapabilities` and cached in the process; a failed load starts a new session with a note (as #1210 did for Hermes),
  never silently continues another engine's id.
- **Model.** `model_for`: the Claude-name guard moves from two places to the table (a row says which names it knows: prefix
  families, or "ACP config option"); the person still sees the note that the default answers.
- **Permissions and cards.** One `Reach` (`ReadOnly`/`Workspace`/`Full`, already in `engines.rs:117`) is what every start
  asks for. `FolderFence` answers whether the engine can hold that reach; Claude's mode words map to `Reach` in one place
  (`bot/hq.rs:42` `codex_reach` generalises). Card handling stays ACP's `session/request_permission`, which is already
  engine-agnostic; the Claude-only parts (`toolName`, option ids) become an optional `ToolIdentity` the driver supplies, and
  a guest card on an engine that supplies none refuses by name instead of falling to `kind` matching. (Guests stay Claude-only
  for now: section 3.)
- **Network.** A `Network` feature, with the answer depending on `owner` and `bypass`: owner in a sandbox gets network,
  anyone else none, bypass has no sandbox. #1229 is the first row of this (Codex terminal). The ACP Codex session and chat
  have no network rule today; whether `codex-acp` accepts one is an open question (section 6).
- **Cost and usage.** `Driver::cost` says how a row's cost reaches the ledger: Claude's hook and transcripts, or the
  `record()` row that `engine_run.rs:208` writes from what the engine reported. The chat path calls the same `record` for a
  turn on a non-Claude engine (today no writer found, section 1.3). Usage windows (`usage.rs:108`) become `Driver::usage`.
- **Account failover.** The chain stays Kith's (`accounts/chain.rs`). What becomes the driver's is the *signal*:
  `Driver::limit` turns a Claude `_claude/rateLimit` update, an ACP RPC error kind and a failed run's text into one
  `Depleted { resets_at }`, so the chain, the host's `mark_out` (`bot/failover.rs`) and the one-shot's `usage_limit`
  (`engine_run.rs:84`) read one type. Accounts exist per engine only where the row has a login dir (`claude`, `codex`
  today); `AccountChain` is `No("no account folders for X")` elsewhere, shown when a chain names it.
- **Session and claim identity.** Today a non-Claude command-line run takes no writer claim unless a door hands one in through the env
  (`exec.rs:496-507`, `start.rs:248-260`); a host chat already runs every engine with claim and lock under
  `kith-stub --resident` (`bot/hq.rs:390`), and `kith-stub` does not care what it wraps (`core/stub/src/main.rs:1-4`). The
  design makes every start (command line included) take the claim and run under `kith-stub`, keyed by the turn's
  conversation: `ClaimLock` is `Yes` for every driver. The thread-writers model has no such writer yet (its `core-term-no-lock`
  row fails `OneWriter`, and engine-run counts as no writer), so **the architect writes that model before phase 4 is coded**.
  The claim row also gets the engine: `terminal/core_claims.rs:235,330` stop writing `"claude"` for every host claim and write
  `engine.id`. One writer per thread holds per engine because the claim is one row keyed by conversation, not by engine;
  a conversation an engine did not make is never resumed by another (the `SessionRef` check above).

### 2.5 Retry and failover per engine

| Engine | Failure | What happens today | After |
|---|---|---|---|
| Claude | usage limit | chat: `limit_seen` then `mark_out`, next account at the next turn; one-shot: `kith_accounts run-chain` walk; **bg ask: single attempt (A2 #1222 decision, follow-up pending)** | same, through `Driver::limit` |
| Codex | usage limit | one-shot reports `usage_limit` JSON (`engine_run.rs:84`); chain walks codex accounts | same |
| Other ACP | usage limit | only if the adapter sets an RPC error kind | `Partial("no usage signal")`; the failure is a plain error, no walk |
| Hermes | any | exit 79 outside bypass; usage not detected | same |
| any | writer already holds the thread | claim refused, exit 10 (`TURN_CLAIM_EXIT`), requeue by the door; two owner runs resuming the same Codex/Hermes session (`KITH_RESUME=<engine>:<id>`) both write it today, no claim | same, and closed by the claim on every start (a bug found in review, recorded in decisions.md) |

## 3. Capability matrix: what each harness supports and what degrades

`Y` yes, `P` partial, `N` no, `R` refused. The right column is how the person sees a degrade. The rule: **a feature is
never silently dropped.** A degrade is a refusal naming the engine and the feature, or a stderr/chat note, both from
`Refusal`/`Notes`.

| Feature | Claude Code | Codex | Other ACP | Hermes | What the person sees |
|---|---|---|---|---|---|
| Terminal session | Y | Y | N | Y | `kith: no terminal session for X; give a task` (exists, `engine_run.rs:237`) |
| One-shot, chat | Y | Y | P (adapter needed) | Y (bypass) | exit 78 text: "X has no ACP one-shot on this computer" (exists) |
| Resume | Y | Y | P (session/load) | Y, new on failed load | a note "starting a new one" (exists, `resume_id`) |
| Model choice | Y | Y | P (config option) | Y | note "default model answers" (exists) |
| Folder fence / modes | Y | Y | N | R outside bypass | refusal sentence (exists for Hermes); a new one for "other ACP: runs in the adapter's own mode" (today silent, `engines.rs:149`) |
| Network in sandbox | n/a | Y terminal (#1229), N ACP | N | n/a | note on the run for ACP Codex: "sandbox network is off here" (new) |
| Tool hooks (guest deny rules) | Y | N | N | N | guests on X refused by name (exists at launch, `plan.rs:1090`) |
| Guests | Y | N | N | N | same |
| Cost row | Y (hook) | Y (one-shot) | Y (one-shot) | Y (one-shot) | chat turns on non-Claude: **gap**, a row is added in phase 5 |
| Usage meters | Y | Y | N | N | no meter shown; "no usage reading for X" in `kith engines` |
| Account chain / failover | Y | Y | N | N | chain entries on X rejected with "no account folders for X" |
| Claim | Y | P (only when a door hands one in; a plain command-line run takes none) | P | P | none today for a bare run; see section 2.4 and the architect's point 3 |
| Lock (`kith-stub`) | Y | host chat: Y (`bot/hq.rs:390`, resident stub, any engine); command line: N | same | same | none on the command line; a claim without the lock is a choice, not a harness limit, and the model does not allow dropping either (architect point 3) |
| `--held` / take-over | Y | N | N | N | "holds claude sessions only" (exists) |
| Wake / detached turn | Y | Y after phase 4 | Y after phase 4 | R outside bypass | today a cancel reason `ENGINE_GAP`; after: engine-agnostic launch, refusal only where the table says |
| Schedules | Y | Y | Y | R outside bypass | a failed turn with the refusal text in the log (79) |
| Asks (fg, bg) | Y | Y | Y | R outside bypass | same |
| Compaction | reported | inferred | inferred | inferred | n/a |

Honest limits this design does not remove: guests stay Claude-only (the hooks and the sandbox are Claude Code's, an
Anthropic-specific setting); Hermes' shell approval is its own and Kith cannot fence it (Kai's decision).

## 4. Phased plan

Order: smallest, least-conflicting first. Every code phase waits for the architect's review of this doc (section 5) and for
Kai to see the design. **Rule for every phase:** keep each file under about 1,500 lines (split `engines.rs`, 1,159 lines now
and growing, into `engines/{table,caps,driver}.rs` in phase 1; `plan.rs` is 1,460 and `settings.rs` 1,618, so a phase that
edits them splits the touched function out first); the old golden tests and `kernel/tests/difftest` corpora must pass
unchanged (rule: behaviour change = changed test); the Rust and Python launches both keep working until R5 deletes the
Python (P1-12).

Open PRs, at their heads: #1220 S2 `4f0488257` (`core_cron.rs`, `sched_turn.rs`, `sched_launch.rs`), #1221 WK4c `644d38e0f`
(`wake_clock.rs`, `channels/*`, `bot/*`, `exec.rs`), #1229 `1de0b1de2` (`tui.rs`, `exec.rs:604`, `engine_run.rs:232`), #1227 R4 (switches the Rust launch on; `bin/kith` is its rollback target).
Merged: #1222 A2, #1228 C3L, #1210 (Hermes refusal), #1219 (prer4: `other_engine` how, Hermes exit 79).

### Phase 1: the table says it, one function reads the agent's engine (S)

- **Changes.** Add `Feature`, `Support`, `RunShape`, `Engine::can` to `engines` with the *existing* facts only (every `can`
  answer is derived from today's `Enforces`, `serves_guests`, `refuses_unbypassed`, `session_modes`; no behaviour change).
  Add `engines::for_run` (the one rule; both doors' current answers become cases of it, so their tests are updated in phase 2, not left to "pass unchanged"). Split `engines.rs` into `engines/{table,caps}.rs`.
  `kith engines --json` and `launch_spec` print the new answers (additive).
- **Files.** `core/kernel/src/engines.rs` (+ new `engines/caps.rs`), `engines/tests`; no caller changes yet.
- **Done when.** A unit test lists every row's `can` answers and equals the pre-change flags; `kith kernel version` lists an
  `engine-caps` token; nothing else changes. Builds on nothing open; conflicts with none (#1229 touches `tui.rs`, not the table).

### Phase 2: replace the copies of "is this Claude?" (S, after #1221 and #1220 merge)

- **Changes.** The two `harness_of_agent` copies and `is_claude` (`core_ask.rs:89,318`, `wake_clock.rs:126,132`) call
  `engines::for_run` (the wake and ask doors' differing rules become one, so `wake_clock_tests.rs:132` and `core_ask_tests` change on purpose); S2's copy of `launch_program` moves to the shared `kernel::launch_program` (a #1222/#1220 loose end); `backups.rs:70`, `plan.rs:1079`, `launch_cmd.rs:145` ask `can(...)`. `ENGINE_GAP`
  stays (same answer, now `can(DetachedTurn)`), so no behaviour moves.
- **Files.** `serve/host/src/routines/core_ask.rs`, `cards/resume/wake_clock.rs`, `routines/core_runs.rs:167`, `routines/sched_launch.rs` (S2),
  `agent/backups.rs`, `core/kernel/src/launch/plan.rs`, `serve/host/src/launch_cmd.rs`.
- **Waits for.** #1221 and #1220 (they edit `wake_clock.rs` and `core_cron.rs` and #1222 left a `TODO(WK4c, #1221)` in
  `core_ask.rs` for this exact dedupe). **Done when** the existing host tests (`wake_clock_tests`, `core_ask_tests`,
  `sched_launch_tests`) pass, with the two pinned disagreements updated, and no `== "claude"` or `eq_ignore_ascii_case("claude` is left outside `engines/` (checked by a test that greps, not by hand).

### Phase 3: refusals in one place (M)

- **Changes.** `engines::check` and `Refusal`; the three Hermes sites (`runonce.rs:91`, `engine_run.rs:286`,
  `bot/hq.rs:63`), the guest refusals (`plan.rs:1090`, `launch_cmd.rs:148`, `agent/hq.rs:199`) and the 78/79 exit codes call
  it. The new text for the one silent case, "other ACP engine runs in the adapter's own mode" (`engines.rs:149`), is added as
  a `Partial` note. Rust only: `bin/kith` is R4's rollback target (#1227, open) and is not edited; R5 deletes it. Difftest
  `intended.json` entries are planned for each behaviour change. `model_for` moves the Claude-name guard out of `plan.rs:958` and `bin/kith:831`.
- **Files.** `engines/caps.rs`, `runonce.rs`, `engine_run.rs`, `agent/bot/hq.rs`, `plan.rs`, `launch_cmd.rs`, nothing in `kernel/bin/kith`
  (rollback target, see above).
- **Waits for.** #1229 (it changes `engine_run.rs` and `tui.rs`; phase 3 then moves its owner-network into `RunShape` and
  `Network`). **Done when** every refusal sentence is byte-identical (golden tests) and `git grep HERMES_REFUSES` shows the
  one constant read in `engines/` only.

### Phase 4: claims and detached turns for every engine (M)

- **Changes.** **Model first**: the architect writes the thread-writers model with an engine-run writer (session id known only after
  `session/new`, run under `kith-stub` keyed by the turn's conversation). Then: every non-Claude start takes the claim and
  runs under `kith-stub`; the permit token is issued on the non-Claude path too (`start.rs:134` is Claude-only, so a Codex
  command-line run makes no cards today); the session registry (`transcripts/mod.rs:139,171`, `rows.rs:145,156`, `held = engine == "claude"`)
  records non-Claude sessions; `--resume THREAD` is allowed for them (`plan.rs:994`, `bin/kith:842`; `plan.rs` is split first,
  1,460 lines); a wake on a non-Claude run is cancelled before the claim when the run is not bypass, using
  `refuses_unbypassed` (the prer4 rule of 2026-11-26), never by reading exit 79 in a report. The conversation's `engine`
  column is routing data, not a label: it is written only on first insert (`external.rs:206-210`) and becomes the bg ask
  turn's engine (`asks/ledger.rs:119-123`), so it means "the engine that made the session", readers prefer the parsed
  session id, and old rows (all `claude`) keep their meaning. The claim row records `engine.id` (`core_claims.rs:235,330`, `external.rs:336`); `SessionRef` parses the
  three stored shapes; the wake turn for a non-Claude agent goes through `kernel launch --exec` like the bg ask already does
  (`core_ask.rs`), so `ENGINE_GAP` answers `Yes` for a row whose `DetachedTurn` is `Yes` and the cancel goes away.
- **Files.** `terminal/core_claims.rs`, `core/runtime/src/conversations/external.rs`, `core/kernel/src/threads/`,
  `cards/resume/wake_clock.rs`, `routines/core_runs.rs`, `launch/plan.rs`, `core/runtime/src/transcripts/`, `launch/start.rs`.
- **Waits for.** The architect's model, #1221 (the wake door by turn id, which is what this extends), #1220 (the launch route), C3L #1228 (merged).
  The `thread-writers` model changes (above), then both models are re-run. Note `ENGINE_GAP` only cancels run-door wakes; bot
  and channel wakes already take the old door on any engine (`wake_clock.rs:179`). **Done when** a wake turn on a Codex agent starts, runs once,
  and ends its claim in the e2e (`launch_turn_e2e_tests.rs` has fake claude/codex engines already).

### Phase 5: usage signal, cost row, account chain (M)

- **Changes.** `Driver::limit` as the one `Depleted` producer (`updates.rs:199`, `client.rs:132`, `chain.rs:14`);
  `Driver::cost` and the chat turn's `record()` for non-Claude engines; `usage.rs:108` becomes `Driver::usage`; the bg ask's
  account walk returns (the A2 follow-up in #1222: one session per attempt, parent mints and records it) as an engine-blind
  walk. Design first (architect point 6): a Claude attempt gets `--session-id` up front, an ACP attempt only after `session/new`;
  a non-Claude attempt records its session after `session/new` and before the prompt is sent, and a resume never crosses engines
  (a walk from a Claude account to a Codex backup starts a new session, with a note).
- **Files.** `core/acp/src/client.rs`, `accounts/chain.rs`, `bot/{updates,failover}.rs`, `usage.rs`, `engine_run.rs`,
  `routines/core_ask.rs`.
- **Waits for.** The A2 follow-up (not started; the architect's corrected design is in #1222's body), and phase 2.
  **Done when** a Codex one-shot, terminal session and chat turn all leave a ledger row, and a usage failure on a Codex callee of a bg
  ask marks the account out and walks.

### Phase 6: the host drops its own copy (M)

- **Changes.** `agent/bot/launch.rs:169` `Adapter` and the wire `Harness` enum become `&'static Engine` plus the driver; the
  `set_*_mode` trio (`bot/hq.rs:297,308,329`) collapses into the driver's `resume`/mode step; Windows: the Unix-only cost
  `record()` follows the kernel's Windows port (W-plan), not this one.
- **Files.** `serve/host/src/agent/{hq,bot/launch,bot/hq,bot/mod}.rs`, `hqmem/oneshot.rs`.
- **Waits for.** Phases 3 and 5. **Done when** `git grep 'Adapter::Codex' serve` is empty and the existing host chat tests
  pass.

## 5. Architect review

Asked once (`kith architect`, answer only), on the draft before this section. Verdict: **approve with changes**; the shape
(capability table plus a few drivers, not a trait per engine) is right. Points condensed, each adopted (A, where) or
answered (R). Where the architect corrected my facts, the doc now says the corrected thing.

| # | Point | Ruling |
|---|---|---|
| 1 MUST | No single rule for which engine runs a turn: the wake door lets the agent's harness win (`wake_clock.rs:125`), the bg ask door lets a non-Claude turn engine win (`core_ask.rs:81-87`), tests pin both; more copies (`launch_cmd.rs:108`, `plan.rs:798`, `bin/kith:294,767`, `core_runs.rs:167`); the backup walk changes the engine after the door checks | A: `engines::for_run` and `check` inside the plan on every attempt (2.3); phase 2 updates the two pinned tests on purpose and lists `core_runs.rs` |
| 2 MUST | A driver depends on engine and kind of run (a Claude chat is ACP; Hermes' MCP and role prompt differ by kind); keyed answers from phase 1 | A: `RunShape` has `kind`, answers keyed by kind (2.2) |
| 3 MUST | Claim and lock facts wrong: a non-Claude command-line run takes no claim without a door's env; host chat has claim and lock under `kith-stub --resident` for any engine; "claim-only is covered by the writer-lock model" is false (`core-term-no-lock` fails `OneWriter`); engine-run counts as no writer | A: matrix and 2.4 corrected; claim plus lock on every start; the architect writes the model before phase 4. My earlier "claim-only is a harness limit" was wrong |
| 4 MUST | Phase 4 misses the `--resume` refusal, the Claude-only session registry, the permit token, the prer4 pre-claim cancel, and that bot/channel wakes already use the old door | A: all in phase 4 |
| 5 MUST | The conversation `engine` column is routing data (first insert only; becomes the ask turn's engine) | A: phase 4 defines it as "the engine that made the session"; readers prefer the parsed session id |
| 6 MUST | Phase 5's walk: Claude has `--session-id` up front, ACP only after `session/new`; #1222's ruling: a fresh non-Claude run records no session | A: phase 5 designs session recording first; a resume never crosses engines |
| 7 SHOULD | `Driver` an enum; pure parts in the kernel, async usage and ACP updates stay in the host | A: 2.2 |
| 8 SHOULD | One table of which features each kind of run checks; static check, then a run-time check after `initialize` | A: 2.2 |
| 9 SHOULD | `Refusal` needs exit-code context (3 vs 4 under a backup walk, 1 vs 4) | A: 2.2 |
| 10 SHOULD | #1227 (R4) is open and `bin/kith` is its rollback target; "difftest unchanged" contradicts phases 4-5 | A: phase 3 edits Rust only and plans `intended.json` entries; #1227 listed in section 4 |
| 11 SHOULD | `RunShape` crosses into engine-run as env; every path sets every field (the bash path does not set `KITH_RUN_OWNER`) | A: 2.2 |
| 12 SHOULD | Windows: the launch is Unix-only | A: `can` takes the platform |
| 13-17 NIT | 28 rows (25 other); `launch_cmd.rs` is a diagnostic verb, not the chat start; the launch-json check is `plan.rs:1079`, the walk `plan.rs:865-913`; a Codex terminal leaves no cost row either; phase 2's grep covers two sites; drop `is_claude_id` | A: counts and citations fixed; `is_claude_id` dropped; the grep test covers `== "claude"` |
| extra | No file conflicts with #1220, #1221, #1229, #1222, #1228 beyond the above | noted |
| extra | A bug found: two owner runs resuming the same Codex or Hermes session both write it (no claim) | A: closed by the claim on every start (2.5); the architect recorded it in decisions.md |

Still open after review: Kai has not yet seen `for_run`; the phase 4 model is the architect's to write; phase 5's session
recording is designed before its brief.

## 6. Open questions for Kai

| # | Question | Options | Recommended default |
|---|---|---|---|
| Q1 | Should Codex serve guests? It lacks tool hooks and a Kith-held sandbox, so a guest on Codex would be held only by Codex's own sandbox | A: stay Claude-only. B: Codex guests with workspace-write and no network, refusing any tool Kith cannot name | A: stay Claude-only; revisit when Codex has hooks |
| Q2 | An ACP engine with no Kith-set modes runs in its adapter's own mode today (silently). Refuse, or run with a note? | A: note on every run. B: refuse non-bypass runs on such engines like Hermes. C: leave as is | A: note. B changes what runs for people today |
| Q3 | Network for the Codex ACP one-shot and chat: owner gets it like the terminal (#1229)? | A: yes if `codex-acp` takes the key. B: terminal only | A, after checking the adapter takes it (not verified here) |
| Q4 | Wake on a non-Claude agent: allow now (phase 4), or keep the cancel until after P2-2? | A: after P2-2. B: in phase 4 | B, because the launch route exists once #1220 and #1221 land; the claim is engine-blind |
| Q5 | Cost rows for chat turns on non-Claude engines (a gap today): record from the engine's reported usage, or leave Claude-only | A: record. B: leave | A: it is the one place where the same agent costs money and the ledger says nothing |
| Q6 | Where do per-engine facts that change with versions live (Codex's two mode spellings, `codex-acp` options) | A: table data in the crate. B: read from the adapter's `initialize` at run time | Both: table says the spellings, the adapter's list confirms (as `codex_mode` does) |
