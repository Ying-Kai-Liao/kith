# Kith gap map: CLI/kernel vs host vs apps

Read-only survey of `main` at `6bfd6faf`, 2026-11-03. Line numbers drift; treat them as pointers.

How to read it
- **CLI**: `kernel/bin/kith` dispatch + usage headers/docstrings of each `kernel/bin/*` script.
- **Host surface**: router methods. `api/mod.rs::route` chains `host, folders, backups, kith_info, marketplace, calls, cards, bots, delegations, threads, people, botguests, peers, remote` (all `serve/host/src/api/*.rs`), plus `chat/topics.rs` (makeTopic/renameTopic...), `limits.rs` (usage methods via `api/host.rs`), `chat/memory/{api,file_api,hq_api}.rs` (via `api/bots.rs`), `chat/handoff.rs`, and the run-socket methods in `runauth.rs`.
- **How**: "calls CLI" means a subprocess of the kernel (the host's helpers are `agent/hq.rs::run/run_as` for `kith ...`, `remote/account.rs::run`/`channels/hq.rs::output` and `cards/*::kith_auth` for `kith-auth ...`, `limits.rs::kith_json`, `wake.rs::run` for `kith-wake`, `doors.rs` for `kith-door`). "own Rust copy" means the host does it itself.
- Also notable: the host binary *is* `kith` (`serve/host/src/kernel.rs::route`): `kith serve <x>` for x not in `start|stop|restart|status|relay|help` runs Rust (`main.rs` clap: pair, info, devices, access, cloud, link, unlink, sample-cards, install, uninstall, stop, update, compat, status, statusline, reset-token, tui, npm, node, memory, mcp). And `kith memory check|log|diff|undo|forget|tidy` goes from `kernel/bin/kith-memory` (HOST_VERBS) into the Rust binary (`hqmem/`). So part of the "CLI" is already Rust.
- Several kernel scripts call the **host's** local API (reverse direction): `kith-call`→`callPerson`; `kith-group`→`bots, updateBot, setBotLimits, sendSecret, roomSecrets, withdrawSecret`; `kith-keys`→`openKeyCard`; `kith-recall`→`summariseThread, forgetThread`; `kith-sessions`/`kith-lifecycle`→`resolveThread`; `kith-permit`→`issueAskToken` + run-socket `requestPermission/requestAccess/consumeAccess/requestSecret`; `lib/kith_runenv.py`→`runEnv`.
- Kernel→host JSON contract (`serve/contract/kernel/contract.json`) covers only `kith ls --json`, `kith project ls --json`, `kith <agent> --launch-json`, `kith ask delegations --json`. Every other CLI output the host parses (`backup list --json`, `account list --json`, `--info-json`, `--backups-json`, `cost ... --json`, `kith-auth people/keys/holders/account-state/ledger/once list --json`, `kith-allow --json`, `kith-wake ls --json`, `kith-door ls`) is untyped on the host side (inferred from the contract's command list).

---

## 1. Agents / packages

| Capability | CLI command | Host/app surface | How the host does it | Notes |
|---|---|---|---|---|
| List agents | `kith ls [--json]` | `syncHqBots` (bots roster); `hello` | calls CLI: `kith ls --json` (`agent/hq.rs::list`, `hub/hq.rs:72`) | In the kernel contract. |
| Agent detail page | `kith <agent> --info-json` (`kith-info`) | `kithInfo` (KithDetails.tsx) | calls CLI (`agent/hq.rs::info`) | Good pattern: read-only, fail-soft, versioned (v1). Not in the kernel contract. |
| Create an agent | `kith new <name> "<desc>"` (`kith-new`) | none for kith agents; `createBot` makes a *host* bot | n/a | App can't create a kith agent. `kith-new` prints no JSON. |
| Install agent from GitHub | `kith install <link> [--as N] [--yes]` | none | n/a | Asks y/N unless `--yes`; no JSON. |
| Browse shared agents | `kith market [words] [--new]`, `kith search [words] [--mine]` | none (app Marketplace is MCP connectors/skills, a different thing) | n/a | `market` is an interactive number picker; no `--json`, unusable by a client. Header of `kith` says "repos tagged hq-agent"; `kith-pkg` says `kith-agent` (or older `hq-agent`). |
| Share/publish | `kith share <agent> [--public [--fresh]]` | none | n/a | Publishes to GitHub; no JSON. |
| Versions, pin, rollback | `kith versions|pin|unpin|rollback <agent> [V]` | none (`kithInfo` shows version/pinned read-only) | n/a | No `--json`. |
| History / restore | `kith history <agent>`, `kith restore <agent> <commit>` | none | n/a | No `--json`. |
| Pull/push/commit agent repo | `kith update|push|commit <agent>|--all` | none | n/a | `kith update` with no agent = self-update (see §12). |
| Missing programs | `kith deps <agent>|--all [--install]` | none | n/a | No `--json`. |
| Validate / private-words check | `kith-pkg check [--share]` (not routed by `kith`; direct script) | none | n/a | |
| Resolve pinned snapshot | `kith-pkg resolve <agent> [--json]` | inside `--launch-json` | calls CLI (indirect) | |
| Who owns this work | `kith who <words> [--json]` | none | n/a | |
| Agent's role/prompt | `kith <agent> --print`, `--inspect` | `kithInfo` (role sections) | calls CLI | |

## 2. Runs and harness

| Capability | CLI command | Host/app surface | How | Notes |
|---|---|---|---|---|
| Run an agent (interactive / one-shot) | `kith <agent> [-p] [-m] [-e] [-H] [--permission-mode|--bypass] [task]` | `send` in a kith-agent bot chat | calls CLI: `kith <agent> --launch-json` then the host starts claude-agent-acp itself (`agent/hq.rs::launch`) | Host runs the session, kernel assembles it. |
| Launch spec for a program | `kith <agent> --launch-json [--hold PID] [--backup ID]` | (internal) | calls CLI | **Only claude and codex** (`kernel/bin/kith:575`): a kith agent with `harness: pi|agy|hermes|qwen|dsh|opencode` can't run from the app. |
| Harness registry | `harness:` in agent.md; `-H`; `kernel/adapters/harness.py` HARNESSES = codex, pi, agy, hermes, qwen, dsh, opencode (+claude) | `agentSetup`, `agentModels`, `agentAuth`, `agentAuthenticate`, `setAgentEnv`, `refreshBackends` (Marketplace AgentSheet, SetupTerminal) | **own Rust copy**: `agent/backends.rs` (27 ACP backends: claude, codex, cursor, pi, omp, opencode, grok, gemini, copilot, qwen, goose, kimi, droid, amp, kilo, cline, auggie, vibe, kiro, devin, qoder, codebuddy, minimax, junie, antigravity, cortex, poolside), `agent/auth.rs` | Two disjoint lists; host's applies to host bots only. Kernel's `agy` vs host's `antigravity`. |
| Set an agent's model / effort / harness on this Mac | **none** (only per-run `-m/-e/-H`, env `KITH_MODEL/EFFORT/HARNESS`, or editing agent.md frontmatter = a new package version, shared) | none for kith agents (`hub.rs:273-282` keeps model/effort/backend from hq; "an app's (stale) copy doesn't get stored") | n/a | Known gap: no `kith <agent> set model|effort|harness`. `kith_settings.py` has no per-agent model key. |
| Permission mode per agent | `kith <agent> set permission MODE` | `kithSetPermission` (HqSettings.tsx) | calls CLI | Writes `~/.kith/settings.json` via `lib/kith_settings.py`. |
| Voice-call model | `kith <agent> set call-model MODEL` | `kithSetCallModel` | calls CLI | Global `call_model` only via `python3 lib/kith_settings.py set call_model MODEL` (no `kith` verb). |
| Ask an agent, get answer | `kith ask <agent> ... [--json]`, `--bg`, `--to`, `--wait`, `--answer`, `ls` | (agent-to-agent inside runs); host bots use `teamCall` (MCP team) | n/a; host team = own Rust (`chat/team/requests.rs`) | Two delegation systems: `kith ask` vs host team requests. `kith ask ls` no `--json`. |
| Delegation records / peek | `kith ask delegations --json` | `delegationPeek`, `delegationsPeek` | calls CLI (`delegation.rs::list`) **+ reads `<machine>/live/ask/<id>.parked` directly** (`api/delegations.rs:78`) **+ copies `HOP_MAX=8`, `CROSS_MAX=2`** from `kith-ask` (`MAX_LOCAL_HOPS/MAX_CROSS_HOPS`) | Constants and a file path duplicated. `chat/team/requests.rs` `MAX_REQUEST_HOPS=8` too. |
| End delegations (stop/revoke) | `kith ask --end [--session S] [--person P]`, `--sweep`, `--cap [N|none]` | `stop`, `revokePerson`, cap card answer via `respondPermission` | calls CLI (`delegation.rs` `ask --end`, `ask --cap`, `ask --cap-told`) | `--cap-told` not in the usage text (inferred internal flag). |
| Auto-mode "Always allow" rules | `kith allow <agent> [--json] [--rm ID] [--derive] [--add-json]` | `allowedRules`, `removeAllowedRule`, auto-allow cards | calls CLI (`cards/autoallow.rs` → `kith-allow`) | Good. |
| Permission prompt / escape cards | `kith-permit` (MCP server, hooks) | `respondPermission`, `issueAskToken`, `revokeAskToken`, run socket `requestPermission/requestAccess/consumeAccess` | both: kernel tool asks host; host decides and writes "always" via `kith-auth access/job/helper` | Kernel→host direction. |
| Doors (guest sandboxed commands) | `kith door prepare|sweep|ls` | (cards `run_door`, `prepare_door`) | calls CLI (`doors.rs` → `kith door sweep`) | `ls` always prints JSON. |
| Phone call to the person | `kith-call [--wait] [--result]` | `callPerson`, `callAccept/Decline/Greet`, `runEnv` | kernel calls host | |
| Deliver result to user | `kith-send ...` | (outboxes; `run_outbox/`) | both: kernel writes outbox, host posts | |
| Push to a bot webhook (Grok) | `kith send-to <service> ... [--json]` | group rooms wake Grok | calls CLI (`chat/group.rs:681` `kith-send-to grok --for serve --body - --json`) | |
| Health-check chain | `kith-chain run|ask|done|close|fail|seen|sig|task` (not routed by `kith`) | none | n/a | No JSON. |
| ACP wrapper | `kith acp <agent> [--keep]` | (host uses its own ACP client) | n/a | |

## 3. Threads / sessions

| Capability | CLI | Host/app | How | Notes |
|---|---|---|---|---|
| Resume an app thread in terminal | `kith <agent> --resume THREAD`, `-r`, `-c` | `resolveThread`, `takeOverThread` | kernel calls host (`kith-sessions resolve` → `/api/resolveThread`) | |
| Session registry + writer claim | `kith-sessions record|claim|terminal|proc|sweep|run` | `terminal/{registry,claim,sync}.rs` | **both**: same file protocol implemented in Python and Rust (`<state>/live/terminal/<sid>.{json,claim}`) | TLA+-checked (thread-writers skill), but two implementations of one protocol. |
| Idle compaction / lifecycle | `kith-lifecycle sweep|status|intent` | host compactor (`memory/compaction.rs while_idle`, per `kith-lifecycle` docstring) | split by owner: kernel for terminal sessions, Rust for host-held ones; host writes intents "in Rust" too | Intent file written by both (per docstring). |
| Message queue / wakes | `kith-wake enqueue|withdraw|withdraw-person|take|done|back|drain|wait|ls [--json]|status` | (internal: turns, Stop, revoke) | calls CLI (`wake.rs`) | `status` no JSON. |
| Chat history, send, stop, unsend, react, markRead, upload, newSession, history, thread, botConversation | **none** (no CLI to read or post into an app chat/thread) | those methods | own Rust (store) | A CLI client can't list threads or post a message except from inside a run (`kith-send`). |
| Thread projects | none (`kith project notes ID...` reads notes) | `threadProjects`, `setThreadProjects` | own Rust | |
| Topics | none | `makeTopic`, `renameTopic`, ... (`chat/topics.rs`) | own Rust | |
| Handoff edits | none | `editHandoff`, `keepConversation` | own Rust | |
| Summarise / forget a thread | `kith summarise --thread ID`, `kith forget --thread ID` | `summariseThread`, `forgetThread` | kernel calls host (falls back to store when host down) | |
| Citations | none | `resolveCitation` | own Rust | |

## 4. Memory

| Capability | CLI | Host/app | How | Notes |
|---|---|---|---|---|
| Search memory | `kith memory search <w> [--agent|--all] [--json]` | `kithMemorySearch` | calls CLI (`agent/hq.rs::memory_search`) | |
| List memory files | `kith memory ls [--agent N --json]` | `kithMemoryList`, `kithMemoryFile` | own Rust (`hqmem::list_files`; `kith_info.rs::resolve_memory_file` reads files directly) | Both read the same files; two listers. |
| Read/save/history/diff/restore | `kith memory check|log|diff|undo` | `kithMemoryRead/Save/History/Diff/Restore` | **Rust owns it** (`hqmem/`, `<state>/memory.db`); the kernel execs `kith memory <verb>` into the host binary | The one area already "shared core in Rust". |
| Tidy-ups (ex "dreams") | `kith memory tidy <agent> [--json]`, `tidy undo` | `kithMemoryTidyUps|Dreams`, `kithMemoryUndoTidy|UndoDream`, `kithMemoryPromotions`, `kithMemoryAnswerPromotion` | Rust (`hqmem/tidy.rs`, `chat/memory/hq_tidy.rs`) | Promotions: no CLI (inferred from usage text). |
| Forget a fact forever | `kith memory forget <text>` | none listed | Rust | |
| Recall index | `kith recall ... [--json]`, `kith memory index rebuild|update` | run socket `recall`, `memorySearch`, `projectId` for guests | calls CLI (`runauth.rs` → `kith-recall`, `kith-memory`, `project-id`) | |
| Host bot memory (non-kith bots) | none | `memory, saveMemory, pinMemory, archiveMemory, forgetMemory, clearMemory, undoTidy, answerMemory, undoMemory, memoryFile, saveMemoryFile, deleteMemoryFile, restoreMemoryFile` | own Rust (`chat/memory/`) | Separate memory model for host bots. |
| Path layout used by memory | `lib/kith_paths.py` | `hqmem::Layout::from_env` | **own Rust copy with drift** (see §14) | |

## 5. Accounts, backups, cost

| Capability | CLI | Host/app | How | Notes |
|---|---|---|---|---|
| Extra logins | `kith account add claude|codex <name>`, `list [--json]`, `remove <h> <n> --yes` | `backups` reads `account list --json`; **no add/remove method** (app shows `fix: "kith account add {h} {n}"`, `api/backups.rs:140`) | calls CLI (read only) | App can't add a login. |
| Main account | `kith account use <id> [--agent A] | --reset | [--json]` | `accountSet` (BackupChain.tsx) | calls CLI | |
| Backup chain | `kith backup list [--json]|add <id> [--agent] [--at N]|remove`, `kith account backup set [ids] | reset --agent` | `backups`, `backupsSet` | calls CLI (`api/backups.rs`) | **Entries are ids only** (`claude`, `claude:<n>`, `claude-api:<p>`, `codex[:<n>]`, `lib/kith_accounts.py`); no model per entry. |
| Chain JSON for a run | `kith <agent> --backups-json`, `--backup ID` | (host moves to backup on usage limit) | calls CLI (`agent/hq.rs::backups`) | |
| Harness sign-in (default login) | none (the harness's own `claude login`/`codex login`) | `agentSetup`, `agentAuth`, `agentAuthenticate`, `setAgentEnv` | own Rust (`agent/auth.rs`, `agent/term.rs`) | Two login models: named account folders (kernel) vs ACP auth for host backends; `setAgentEnv` keys go in host store. |
| Plan usage (subscription windows) | none | `usage` (+ `kith serve statusline`) | own Rust (`usage.rs` runs `claude -p /usage`) | No CLI equivalent. |
| Cost at API prices | `kith cost [today|week|month|all]`, `agents`, `agent <n> [--json]`, `people`, `spent`, `session`, `usage --json`, `limit <a> --json`, `members <a> --json`, `bots --json`, `sync`, `csv`, `tag`, `record` | `usageReport`, `agentUsage`, `agentPeople`, `botsCap` (UsageLimits, SpendSection, PeopleBudgets) | calls CLI (`limits.rs::kith_json`) | Good. |
| Agent monthly limit | `kith <agent> set limit N|none`, `set at-limit ask|tell|hold` | `setAgentLimit` | calls CLI | |
| Person budget on agent | `kith auth member <a> <p> budget=USD` | `setPersonBudget` | calls CLI (`kith-auth member`) | |
| Bots cap | `kith auth bots-cap <amount|off>` | `setBotsCap` | calls CLI | |
| Backup of state to GitHub | `kith backup [--yes|--check|--json|--nightly [off]]` (`kith-state`) | none | n/a | **Footgun: bare `kith backup` backs up and pushes, creating missing private repos.** |

## 6. Auth, secrets, keys

| Capability | CLI | Host/app | How | Notes |
|---|---|---|---|---|
| Grants per agent/service | `kith auth ls|grant|granted|revoke|holders [--json]` | `revokeKey`; Key cards grant | calls CLI (`cards/secret.rs` → `kith-auth grant/revoke/holders --json`) | `ls`, `granted` have no `--json`. |
| Store a secret | `kith auth set <profile> <VAR>` (prompt), `--stdin-json --grant A` | `provideSecret`, `openKeyCard` (KeysView) | calls CLI (`kith-auth set --stdin-json`) | |
| Ask owner for a key from terminal | `kith keys add <svc> --profile P [--for A] [--json]` | opens a card | kernel calls host `openKeyCard` | |
| List keys (no values) | `kith auth keys --json` | `listKeys` | calls CLI | |
| One-time room secrets | `kith group secret|secrets|secret --withdraw` → host; `kith-auth once put|copy|take|drop|list` | `sendSecret`, `roomSecrets`, `withdrawSecret`, `botSecretFetch` | host calls `kith-auth once ...`; `kith group` calls host | Round trip CLI→host→CLI. |
| Account state for a card | `kith auth account-state <a> <svc> --json` | Key card | calls CLI | |
| Service env per agent | `kith auth env <agent> <svc> [VAR=v]` | none | n/a | |
| Browser profiles | `kith auth browser <name> [browser]` | none | n/a | |
| Run with secrets | `kith-auth exec|get|mcp-config|mcp-names|helper-mcp` | `rich_card/jev.rs:614` uses `kith-auth get --for serve jev-browser TYPESAFE_API_KEY` | calls CLI | |
| **Second secret store** (connector env/headers, website logins, Composio key, 1Password token, voice key, Jev router key) | **none** | `credentialUpdateConnector`, `credentialStatus`, `credentialLogins`, `credentialSaveLogin`, `credentialRemoveLogin`, `credentialSetOnePassword`, `setComposioKey`, `setVoiceKey`, `setRouterKey` | **own Rust copy**: `market/vault.rs` (SQLite ciphertext, master key in Keychain), `market/logins.rs`, `market/passwords.rs`, `chat/router.rs` | **The Jev key lives in both**: `chat/router.rs` reads `JEV_API_KEY` env else vault slot; channels/`rich_card/jev.rs` read kith-auth `jev-browser/TYPESAFE_API_KEY`. Notion etc. exist as kernel services (`kernel/services/notion.json`) and could also be installed as Marketplace connectors. |
| auth.json change detection | (kith-auth owns the file) | card sweeps | host stats `secrets/auth.json` mtime (`cards/mod.rs:813`), with its own copy of the path rules | Read-only, but path copy. |

## 7. People, invites, bot guests

| Capability | CLI | Host/app | How | Notes |
|---|---|---|---|---|
| People list | `kith auth people [ID] [--matrix|--json]` | `people` (PeopleView) | **both**: rows from host store (`hub.store.people()`, devices, memberships, invites); only `revoked` comes from `kith-auth people --json` (`api/people.rs:285`) | Dual source: a person/member added by CLI may not show in the app (inferred from `people()` building rows from the store). |
| Invite a person to the app | none (`kith auth invite <id> [min]` is a *channel* `/link CODE`, different) | `createInvite`, `invites`, `revokeInvite`, `proveInvite` (InviteJoinSheet) | both: host store + cloud + `kith-auth person/member/retired/person --rm --unused` | No CLI to create/list/revoke app invites. |
| Grant agent access | `kith auth member <a> <id> [preset= mode= deny= model= helpers= budget= env.X=] [--trusted] [--rm]`, `preset` | `setPersonAccess` (AccessTab) | calls CLI (`kith-auth member preset=<role> budget=`) | Note `member ... model=` exists for a person's row but not for the owner's agent. |
| Revoke / unrevoke person | `kith auth person <id> --revoke|--unrevoke [--local]` | `revokePerson`, `unrevokePerson` | calls CLI (`--local`) + host does cloud fan-out; CLI on a cloud-linked Mac goes through the host | Two entry points, one rule (revoke-access TLA+). |
| Grants ledger | `kith auth ledger [--json]`, `ledger --rm KEY` | `accessLedger`, `removeGrant` (AccessGrants) | calls CLI | |
| Escape-card "always", jobs, helpers | `kith auth access|jobs|job|helper|door|may|resolve` | cards (`cards/access.rs`, `cards/edge.rs`), `roles.rs` guard | calls CLI | |
| Audit log | none | `audit` | own Rust (store) | |
| Start chat with a person | none | `startChat` | own Rust | |
| Bot guests (Grok, etc.) | `kith auth bot <id> --of <p> [--rm]`, `kith group add-bot|remove-bot`, `kith bot set <bot> [--wake-profile] [--turn-timeout]` | `createBotInvite`, `revokeBotInvite`, `revokeBot`, `bots`, `myBots`, `decideBot`, `setAutoAllowBots`, `setBotLimits`, bot side: `botAsk, botPost, botRoomRead, botSecretFetch, botThread, botConversations, botStop, botLeave, botStatus, proveBotInvite` (BotGuestsView) | both: host store (`store/botguests.rs`) + `kith-auth bot/member preset=guest/bot --rm`; `kith group`/`kith bot` call host | No CLI for bot invites, decide, auto-allow, list `myBots`. |
| Handles / channel linking | `kith auth handle|invite|link|whois|who` | channels `/link CODE`; cloud account binding | calls CLI (`channels/hq.rs`, `remote/account.rs`, `remote/members.rs`) | |
| Rename person | `kith auth person <id> id=<new>` | none | n/a | |

## 8. Channels (Telegram, Discord, Slack)

| Capability | CLI | Host/app | How | Notes |
|---|---|---|---|---|
| Which bots run | `kith auth grant serve telegram|discord|slack <profile>` | none (app) | host polls kith-auth grants each minute (`channels/manager.rs`) | Configured through the CLI, run by the host. |
| Channel settings (dm_default, debounce_s, router, interrupt, context_show, permission_mode, discord_guild) | **none** | **none** | own Rust: `<data dir>/config.json` `channels` object, read at start (`channels/mod.rs` header) | Hand-edit only; neither CLI nor app. |
| Person lookup / link | `kith auth who|link` | channels | calls CLI | |
| Turn execution | `kith <agent> <task>` | channels run | calls CLI (`channels/run.rs` subprocess) | |
| Router key (Jev) | `kith auth grant serve jev-browser <p>` | `routerStatus`, `setRouterKey` | **both, two stores** (see §6) | |
| Off for non-default home | env `KITH_SERVE_CHANNELS=on|1` | `/health` `channels.off` | own Rust | |

## 9. Marketplace: connectors, skills, credentials, Composio, ACP engines

All Rust (`serve/host/src/market/`, 3.6k lines), no CLI, and **not given to kith agents** (`agent/hq.rs` test `launch_json_becomes_claude_options_and_nothing_of_serves` asserts no chat/memory/team/routines/connectors servers on a kith agent's session).

| Capability | CLI | Host/app | How | Notes |
|---|---|---|---|---|
| Browse MCP Registry | none | `marketConnectors` (MarketplaceView) | own Rust `market/registry.rs` | Kernel equivalent is `kernel/services/*.json` (12 services) + `capabilities.yaml needs:`. |
| Install/remove/import connectors | none | `installConnector`, `removeConnector`, `importConnectors`, `connectors`, `connectorInfo`, `connectorTarget`, `connectorRuntime` | own Rust `market/install.rs`, `market/updates.rs` | |
| Connector sign-in (OAuth 2.1/PKCE) | none | `connectorSignIn`, `connectorSignInFinish`, `connectorVerify`, `connectorRequestFinish` | own Rust `market/oauth.rs`, `market/verify.rs`, `market/requests.rs` | |
| Call a connector tool | none | `connectorCall` (MCP `kith-connectors`) | own Rust | |
| Credentials | none | `credential*` (6 methods) | own Rust vault | See §6. |
| Composio | none | `composioStatus`, `setComposioKey`, `composioToolkits`, `composioConnect`, `composioConnectFields`, `composioConnection`, `composioCall` | own Rust `market/composio.rs` | |
| Skills catalog (anthropics/skills) | none (kernel: `skills.json` + `kith-pkg fetch`, `kith-pkg promote`) | `marketSkills`, `skills`, `skillDetails`, `installSkill`, `removeSkill` (SkillSheet) | own Rust `market/skills.rs`, stored `~/.kith/serve/skills/<id>` | Two skill systems: per-agent pinned `skills.json` (kernel) vs per-computer host skills for host bots. |
| ACP engines (install/sign in) | none | `agentSetup`, `agentAuth*`, `agentModels`, `refreshBackends` (AgentSheet, SetupTerminal) | own Rust `agent/backends.rs`, `agent/auth.rs`, `agent/term.rs`, `agent/node.rs` | See §2. |

## 10. Settings

| Capability | CLI | Host/app | How | Notes |
|---|---|---|---|---|
| Per-agent permission / call model / limit / at-limit | `kith <agent> set permission|call-model|limit|at-limit` | `kithSetPermission`, `kithSetCallModel`, `setAgentLimit` | calls CLI | |
| Global defaults (permission_mode, plugins, call_model, backups, account) | **no `kith settings` verb**; `python3 kernel/lib/kith_settings.py KEY` / `set call_model MODEL`; `kith backup`/`kith account use` cover two keys | none | n/a | Default permission mode and `plugins` have no setter at all except hand-editing `~/.kith/settings.json`. |
| Delegation chain cap | `kith ask --cap [N|none]` | cap card | calls CLI | |
| Host-only switches | none | `setAnalytics`, `track`, `setScreenEnabled`, `setJevCards`, `setHostAutomaticUpdates`, `setApproval`, `setCloud`, `setAutoAllowBots`, `setVoiceKey`, `setRouterKey` | own Rust (store kv / `~/.kith/serve/config.json`) | Some reachable via `kith serve cloud ...` (Rust). |
| Relay URL | `kith serve relay [URL]` | (cloud settings) | **kernel writes host-owned file**: bash `kith-serve` reads/writes `~/.kith/serve/config.json` `relay_url` | Reverse copy. |

## 11. Scheduling

| Capability | CLI | Host/app | How | Notes |
|---|---|---|---|---|
| Kith-agent cron jobs | `kith schedule ls|add|rm|pause|resume|run|log|prune|tick|--install|--uninstall` (`kith-sched`, jobs in `<agents>/<a>/jobs.json`, launchd `dev.kith.sched`) | `kithSchedule` (op pause/resume/run/remove only; jobs read from `--info-json`) | calls CLI (`agent/hq.rs::schedule`, run as the job's person) | **App can't add or edit a kith job.** No `--json` on any `kith-sched` verb. |
| Host bot routines | **none** | `routines`, `saveRoutine`, `setRoutineEnabled`, `deleteRoutine`, `runRoutine`, `routineSchedule`, `routineWebhook`, `routineCall` (Routines view, MCP `kith-routines`) | **own Rust**: `routines/` (croner, store, webhooks, events) | Two schedulers with different cron dialects (kith-sched: local time, `@daily`; routines: IANA tz, intervals, once, webhook/event triggers). |
| Scheduled messages (cloud) | none | `scheduleMessages` | own Rust | (inferred purpose) |

## 12. Remote machines, cloud, devices

| Capability | CLI | Host/app | How | Notes |
|---|---|---|---|---|
| Other machines over ssh | `kith remote add|ls|rm`, `kith ask agent@machine` | none | n/a | `ls` no `--json`. |
| Mac link (pair two Macs on one account) | `kith link [NAME] [--accept [CODE]|--deny]`, `kith unlink NAME` | `linkComputer`, `linkStatus`, `peerComputers`, `peerRequests`, `decidePeerRequest`, `unlinkComputer`, `peerAgents`, `peerRevokes` (ComputersView) | Rust both ways (`kith link` → host binary `Link`) | Third remote concept: desktop app's own SSH profiles/tunnels (`apps/desktop/src/main/ssh.ts`, SSHRow). |
| Cloud on/off, owner, approval | `kith serve cloud [...] [--confirm-owner]` (Rust) | `cloudStatus`, `setCloud`, `setApproval`, `confirmOwner`, `claimSign`, `unclaim`, `linkCode` | own Rust (`remote/`) | CLI path exists but is host code. |
| Devices / access requests / pairing | `kith serve devices [list|revoke]`, `kith serve access [list|approve|deny]`, `kith serve pair [--json]` (Rust; routed because not in KERNEL_SERVE_VERBS) | `devices`, `revokeDevice`, `registerDevice`, `unregisterDevice`, `registerActivity`, `unregisterActivity`, `pairing`, `accessRequests`, `decideAccessRequest` | own Rust | Not listed in `kith --help`. |

## 13. Doctor, updates, install, misc

| Capability | CLI | Host/app | How | Notes |
|---|---|---|---|---|
| Health check | `kith doctor` | none (app has MacProblemCard; host `hello.store` health) | n/a | No `--json`; a client can't show doctor results. |
| Update kith | `kith update` (`kith-self-update`: git pull / brew upgrade / install.sh / app link), `--check` | `hostUpdateStatus`, `checkHostUpdate`, `installHostUpdate`, `setHostAutomaticUpdates` | **own Rust** (`update/`), separate updater | Two updaters; packaged installs ship the kernel beside the binary (`kernel.rs::find_kernel`), git installs don't (inferred: who updates what differs by layout). **Footgun: bare `kith update` acts.** |
| Set up a Mac | `kith init [--from U] [--yes]` | none | n/a | |
| Migrate hq→kith | `kith migrate [--dry-run|--undo|--force|--status]` | none | n/a | **Footgun: bare `kith migrate` performs the move.** |
| Host service | `kith serve start|stop|restart|status` (bash) / `install|uninstall|status` (Rust) | `hello`, `/health` | both | |
| Projects | `kith project [ls --json|add --json|root|notes|open]`, `project-id ...` | `projects`, `registerProject` | calls CLI (`kith project ls --json`, `kith project add`) | `recentFolders`, `listDirs`, `setPlaceFolder` are host-only. |
| Bug reports | `kith bug "<t>"|list|show|send|publish` | `reportError` (crash/analytics, different) | n/a | `list/show` no `--json`. |
| Voice, screen, computer use | none | `voice*` (8), `voiceLocal*` (3), `screen*` (6), `requestScreenPermission`, `computerTools`, `computerCall` | own Rust | Host-only by nature; fine. |
| Terminal relay for setup | none | `termInput`, `termResize`, `termClose` | own Rust | |
| Sample cards | `kith serve sample-cards` (Rust) | `sampleCards` | own Rust | |
| Rig/test | — | `api/rig.rs` | test only | |

## 14. Kernel-owned files the host touches directly (copies)

| What | Kernel owner | Host copy | Drift? |
|---|---|---|---|
| State/home folder rules | `kernel/lib/kith_paths.py` | `serve/host/src/kernel.rs::machine_dir/home_dir`; `serve/host/src/hqmem/mod.rs::Layout::from_env` | **Yes.** `machine_dir` follows `.migrated` and the `~/.hq` real-dir/symlink cases like kith_paths. `Layout::from_env` picks `~/.kith` whenever it exists, else `~/.hq`, ignoring `.migrated`; `home_dir` ignores kith_paths' "folder around the kernel" rule. Comments say commands pass paths explicitly, so it bites on bare `kith memory log` (Rust fallback). |
| auth.json location | `kith-auth` | `cards/mod.rs::auth_stamp` (mtime only; 3 candidate paths) | path list copy |
| Terminal session registry + claims | `kith-sessions` | `terminal/registry.rs`, `terminal/claim.rs` | dual implementation of one protocol |
| Delegation parked marker | `kith-ask` (`live/ask/<id>.parked`) | `api/delegations.rs::parked_in` | path copy |
| Delegation hop caps | `kith-ask` `MAX_LOCAL_HOPS=8`, `MAX_CROSS_HOPS=2` | `api/delegations.rs` `HOP_MAX`, `CROSS_MAX`; `chat/team/requests.rs` `MAX_REQUEST_HOPS` | constant copy |
| memory.db + memory files | Rust `hqmem` (kernel delegates) | — | inverted ownership (fine, but it's the Rust core in disguise) |
| Host config.json | host | bash `kith-serve relay` writes `relay_url` | reverse copy |
| jobs.json | `kith-sched` (only writer) | host reads via `--info-json` only | OK |
| ~/.kith/settings.json | `kith_settings.py` | host uses CLI | OK |

---

## Top gaps that matter (ranked)

1. **Two agent models.** Host-native bots (ACP backends `agent/backends.rs`, `market/` connectors and skills, `routines/`, `chat/memory`, host team requests) vs kith agents (kernel). Kith agents get none of the Marketplace/routines/team stack (stripped in `agent/hq.rs`), and none of that stack has a CLI. Every other duplication below follows from this.
2. **Two secret stores.** `kith-auth` (auth.json + Keychain refs, `kernel/services/*.json`, per-agent/per-person grants) vs `market/vault.rs` (SQLite ciphertext, Keychain master key) for connector env/headers, logins, Composio, 1Password, voice key, Jev router key. The Jev key is read from both (`chat/router.rs` vault slot vs `rich_card/jev.rs` `kith-auth get --for serve jev-browser TYPESAFE_API_KEY`). The vault has no CLI and no person/agent grant model.
3. **Two schedulers.** `kith-sched` (jobs.json, launchd) vs host `routines/` (croner, webhooks/events). The app can only pause/resume/run/remove kith jobs (`kithSchedule`); it can't add or edit one. The CLI can't see routines. `kith-sched` has no `--json`.
4. **Model/effort/harness is not settable per agent, and the harness lists differ.** No `kith <agent> set model|effort|harness`; the app can't edit them for kith agents (`hub.rs:273-282`); `--launch-json` refuses anything but claude/codex (`kernel/bin/kith:575`), so kith agents on pi/agy/hermes/qwen/dsh/opencode can't run in the app; `adapters/harness.py` (8) vs `agent/backends.rs` (27, `agy` vs `antigravity`).
5. **Backup chain entries carry no model** (`lib/kith_accounts.py` ids, `kith backup add <id>`, `kith account backup set`, `kith_settings.py backups`). Fix the kernel schema first (e.g. `claude:work@sonnet` or an object entry). The app's BackupChain only mirrors it.
6. **Path-rule copies with real drift** (`kith_paths.py` vs `kernel.rs::machine_dir` vs `hqmem::Layout::from_env`), plus host reads of kernel files (`live/ask/*.parked`, `live/terminal/*`, auth.json mtime) and the kernel writing host `config.json`. Small to fix, silent when it breaks.
7. **People/invites have two sources.** Host store (people, memberships, invites, devices, audit) plus kith-auth (people, members, revoked). App invites (`createInvite/invites/revokeInvite`) and bot invites (`createBotInvite/decideBot/setAutoAllowBots`) have no CLI. A CLI-made member may not appear in the app's People list (inferred).
8. **Logins: `kith account add/remove` has no app method.** The app signs in host backends through ACP terminals (`agentSetup/agentAuth`) and tells the user to run `kith account add`. Plan usage (`usage`) is host-only, with no CLI.
9. **Protocols implemented twice.** Writer claim (`kith-sessions` + `terminal/claim.rs`), lifecycle intents (`kith-lifecycle` + host stop paths), delegation hop caps (3 copies), two delegation systems (`kith ask` vs host `teamCall`). TLA+ covers the claim; the constants have no check.
10. **Package lifecycle is CLI-only, without JSON** (`kith market|search|install|new|share|versions|pin|history|restore|deps`). `market` is an interactive picker, so the app can't install, version, roll back or create kith agents. Only 4 kernel outputs are in `serve/contract/kernel/contract.json`; the host parses ~15 more untyped.

Runners-up: channel settings exist only as hand-edited `config.json`, with no CLI and no app surface; no global `kith settings` verb (default permission mode, plugins); two updaters (`kith update` vs `installHostUpdate`); three "other computer" concepts (`kith remote`, Mac link, desktop SSH); `kith doctor` has no JSON.

## CLI commands lacking `--json` that a client would need

- `kith schedule ls|add|rm|pause|resume|run|log` (`kith-sched`: none have it; the host reads jobs from `--info-json`)
- `kith versions <agent>`, `kith history <agent>`, `kith pin|unpin|rollback|restore` (result)
- `kith market [words]`, `kith search [words]` (also need a non-interactive listing; `market` reads a number from stdin)
- `kith install <link>` (result + the "what it would do" preview), `kith new`, `kith share`, `kith deps`
- `kith update|push|commit <agent>|--all` (per-agent result)
- `kith doctor`
- `kith remote ls`, `kith remote add`
- `kith ask ls`
- `kith bug list`, `kith bug show <id>`, `kith bug "<title>"` (the new id)
- `kith auth ls`, `kith auth granted`, `kith auth grant|revoke` (result)
- `kith backup add|remove` (list has `--json`; writes print text)
- `kith <agent> set permission|call-model|limit|at-limit` (no result; host re-reads `--info-json`)
- `kith update --check` (one text line)
- `kith-wake status <id>`, `kith-lifecycle status <sid>`, `kith-chain` (all verbs)
- `kith project` (bare: the project here), `kith project notes`
- `kith-sessions pick|latest` (interactive / plain id)
- Already fine: `kith ls`, `kith project ls|add`, `kith account list|use`, `kith backup list|--check`, `kith cost usage|agent|limit|members|bots`, `kith memory ls|search|log|diff|tidy`, `kith recall`, `kith allow`, `kith who`, `kith ask`, `kith-wake ls`, `kith door ls` (always JSON), `kith-auth people|keys|holders|account-state|ledger|jobs|budgets|once list`, `kith group`, `kith keys add`, `kith send-to`.

## Footguns (bare or near-bare subcommands that act)

- `kith backup` (bare): backs up everything to private GitHub repos and pushes, creating missing repos. `kith backup --check` is the read-only form.
- `kith update` (bare): updates kith itself (git pull --ff-only, `brew upgrade`, or reruns `scripts/install.sh`). `kith update --all` also updates every agent.
- `kith migrate` (bare): runs the ~/.hq→~/.kith move and swaps the launchd jobs. Use `--dry-run` / `--status`.
- `kith init` (bare): interactive setup; offers are skipped without a TTY.
- `kith <word>`: any word that isn't a subcommand is an agent name; `kith <agent>` with no task starts an interactive Claude session.
- `kith schedule run A ID`, `kith deps --install`, `kith share --public`, `kith-state commit` (pushes), `kith door sweep`, `kith ask --end/--sweep`: all act immediately.
- `kith cost` (bare = `agents`) syncs the ledger first (writes `~/.kith/cost.db`, harmless).
- Safe bare forms: `kith auth` (= ls), `kith schedule` (= ls), `kith wake` (= ls), `kith serve` (= help), `kith ask|allow|account|memory|recall|remote|keys|who|bug|door` (= --help).

## Architect's view

Asked once, one-shot (`KITH_OUTPUT=json kith architect "..."`, Opus, answer-only), with the draft top-10 above. Its answer, condensed and attributed to the architect agent:

- **Reorder.** Put the path drift first: it's the only gap where code gives a wrong answer today, and every shared crate needs paths first. It cites `hqmem/mod.rs:56-59` vs `kernel.rs:117-122` / `kith_paths.py:51`, and a test at `kernel.rs:240` covering the Mac where they disagree (`~/.kith` holds only serve data, real state in `~/.hq`). Put secrets second and fold accounts (#8) and people/invites (#7) into it: all three are "who holds a credential, and who can grant or revoke it". Two stores mean two revoke paths, and the secret-cards model covers only the host card path. It flags as its own inference that people/invites are dual-written rather than cached. Put the writer claim third: an order-dependent protocol written twice is how you get two writers.
- **The two agent models are a product decision, not a gap.** Should host-native bots become kith agents, or stay a separate kind? That has to be settled before schedulers (#3) and harness/model (#4) can be fixed properly. Fold backup-model (#5) into #4; it says the live backup problem is account pinning ("bug 404e"), not the model. Move package lifecycle (#10) to last: CLI-only fits the principle, so the only gap is `--json`.
- **Shared Rust core first:** paths, then writer claims and hop caps, then one secret-store API. Follow the hqmem precedent: Rust subcommands in the one `kith` binary, called by the Python kernel. These run on every turn, involve locking, or are security-critical.
- **Stay Python, host calls the CLI with `--json`:** package lifecycle, agent settings (a new `kith <agent> set model|effort|harness`), the harness registry via `--launch-json`, people/invite commands, the must-ask list (as `kith allow --derive` already is). These are rare, git-heavy admin jobs.
- **Schedulers:** `jobs.json` stays the one place jobs are defined; the always-on host can be what runs them. Rule for every pair: each fact has one owner, and the other side reads through it and never keeps its own copy.
- Correction to the architect: it couldn't find the kernel's delegation cap. It is `kernel/bin/kith-ask:89-90` (`MAX_LOCAL_HOPS = 8`, `MAX_CROSS_HOPS = 2`), copied in `api/delegations.rs` (`HOP_MAX`, `CROSS_MAX`) and `chat/team/requests.rs:16` (`MAX_REQUEST_HOPS`). I did not verify its "bug 404e" reference or its `kernel.rs:240` test citation.
