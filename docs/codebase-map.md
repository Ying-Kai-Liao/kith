# Codebase map

Where every part of kith lives, what each file does, and which file to open for a given change. The model and the
reasons behind it are in [architecture.md](architecture.md); commands are in the [README](../README.md).

## Where things are on disk

```
~/kith -> kith/                       the kith folder (KITH_HOME), wherever it really is (an external disk)
kith/                               this repo (public): README, docs/, install.sh, .claude-plugin/
  docs/                             architecture.md (the model), this map, how-hq-runs.html (+ .architecture.json,
                                    its source) and img/how-hq-runs-{light,dark}.png (the README's picture)
                                    ci.md (CI jobs, path filters, the merge queue)
kith/kernel/                        the code: what kith is (bin/, lib/, adapters/, PROTOCOL.md, services/)
kith/agents/                        the agents folder, $KITH_AGENTS: your private repo (<you>/hq-agents), each agent's life
  <name>/                           one agent
    role/                             what it is: its own git repo (<you>/kith-agent-<name>), ignored by hq-agents
      agent.md                          frontmatter (description, model, effort) + instructions
      capabilities.yaml                 tools, needs: [services], global_skills
      settings.json                     Claude Code settings merged over yours for its runs
      skills/  skills.json  tools/      own skills; third-party skills by link; its CLIs
    memory/                           MEMORY.md, projects/<id>.md, sessions/<project>.md
    data/                             its databases (*.db ignored; committed as *.sql text dumps)
    files/<project>/                  what it made: renders, images, exports, backups (ignored: on disk only)
    skills/learned/                   skills kept private (kith-pkg promote moves one into role/)
    jobs.json                         its scheduled jobs (kith-sched)
~/.kith/                            this Mac's kith, a real folder on the internal disk (like ~/.claude)
  user.md                           committed (the hq-state repo, $KITH_STATE): who kith works for, read by every agent
  projects/<id>/                    committed: codebase.md, verify.md, decisions.md, per-project notes shared by agents
  projects.json                     committed: project registry (bin/project-id): aliases, parent, home
  agents.json                       committed: installed agents and where their roles come from (kith-pkg save)
  private-words                     committed: words kith-pkg check --share refuses in a role
  .gitignore                        ignores everything else (kernel/lib/dot-kith.gitignore): from here down, this Mac only
  secrets/                          auth.json (grants and profiles, kith-auth), auth.log, env (Telegram, tokens);
                                    agents' file tools are denied it (bin/kith)
  settings.json                     kith's own settings for this Mac, not secret (lib/kith_settings.py): permission_mode, plugins, agents
  ask.json  remotes.json            kith ask conversations → sessions; other machines (kith-remote)
  cost.db                           the cost ledger (kith-cost)
  bridge/  sched/                   the retired Python bridge's state (the host imports it once); last tick and runs per job
  monitor/  house/  cost/           devops' monitor.db and monitor.log; house's remind.log; the cost sync.log
  store/<name>@<version>/           read-only role snapshots runs are pinned to (prompt.md, settings.json, .home);
  store/.pointers/                  unchanged files hard-linked to the previous version's; good/pin per agent
  logs/                             sched/tick.log, sched/runs/<agent>/ (each scheduled run's output)
  cache/                            rebuildable: handoff/ (scratch clones and worktrees of callers' repos, kith-remote)
  bin/kith-uv                         uv copy with Full Disk Access, what every launchd job runs (repo on an external disk)
~/Workspace/kith -> kith/kernel/      older address of the kernel, kept for paths that still name it (removable)
```

Every tool asks one resolver, `lib/kith_paths.py` (bash: `eval "$(python3 lib/kith_paths.py --sh)"`; the plugin runs
it with `--json`). `KITH_HOME` is the variable, else `~/kith`, else the kernel's folder; agents are `<home>/agents`.
`~/.kith` gives the rest: `KITH_STATE` (itself), `KITH_MACHINE`, `KITH_SECRETS`, `KITH_STORE`, `KITH_LOGS`, `KITH_CACHE`. While
`~/.kith` is still a link to the kith folder (the layout before 2026-10-15) it answers the old places instead:
`<home>/state`, `~/.config/kith`, `<agents>/.store`, `~/Library/Logs/kith`, `~/Library/Caches/kith`.
It gives each folder path twice: the address (no symlinks resolved, so no volume name with spaces reaches prompts
or Bash rules) and `_REAL` (resolved; what Claude Code matches file permission rules against). They are outputs,
exported for agents' tools, not settings. An agent's role is `<agents>/<name>/role` and its life the folder around
it; an agent without `role/` is the older layout (the folder is the role, the life is `<state>/agents/<name>`).
Agents' settings and tools name these through placeholders and env vars, never `~/.config/kith`.

## Background jobs

LaunchAgents in `~/Library/LaunchAgents/`, each run through `~/.kith/bin/kith-uv`, by the `~/kith` address (not the disk path).

| Label | Runs | When | Written by |
|---|---|---|---|
| `dev.kith.sched` | `kernel/bin/kith-sched tick` | every minute | `kith-sched --install` (not installed on this Mac) |
| `dev.kith.cost` | `kernel/bin/kith-cost sync` | every 6 h | by hand (no installer) |
| `dev.kith.monitor` | devops `role/tools/monitor.py run` | always (KeepAlive) | `monitor.py install` (devops' repo) |
| `dev.kith.house` | house `role/tools/house.py remind --send` | 08:00 daily | by hand (no installer) |

## Kernel files

Paths in this table are inside `kernel/`, except `install.sh` and `.claude-plugin/marketplace.json` (top of the repo).

| File | Lang | What it does | Called by |
|---|---|---|---|
| `bin/kith` | bash | Runs one agent: resolves the pinned package, fills placeholders, adds memory and project, settings + cost-tag hook, granted MCP servers, model/effort, then `exec claude`. Also the front door for `versions`, `install`, `auth`, `cost`… | you, `kith serve`, agents calling helpers |
| `bin/kith-pkg` | python | Packages: fingerprint → check → snapshot into `~/.kith/store` (clones or hard links), gc; pin/rollback; auto-commit each good version to the agent's repo; install/update/share/search/restore; fetch third-party skills; `check --share` privacy scan | `bin/kith`, the adapter (`resolve --all --json`), `install.sh` |
| `bin/kith-auth` | python | Services and grants: which agent may use which service as which account; builds `--mcp-config`; hands out secrets (`get`, `exec`) and logs them; keeps the "always" answers to escape cards (`access`); `ledger [--person] [--agent] --json` lists every lasting grant for anyone but the owner (those answers and people's own service grants) with a stable key and when each was given, `ledger --rm <key>` takes one away: the host's `accessLedger`/`removeGrant`, the apps' Access list | `bin/kith`, agents' tools, the host |
| `bin/kith-cost` | python | Cost ledger: syncs request usage from `~/.claude/projects` into `cost.db`; `tag` records each kith session (SessionStart hook); reports by agent, version, model, effort | you, launchd `dev.kith.cost`, the hook `bin/kith` adds |
| `bin/kith-sessions` | python | Continue anywhere: `record` (SessionStart hook) lists a `kith <agent>` terminal session in `$KITH_STATE/live/terminal/<sid>.json` for the host; `resolve` backs `kith <agent> --resume <thread>`; `run` is the claude child that prints the take-over note | the hook and the launch tail of `bin/kith` |
| `bin/kith-ask` | python | One-shot `kith <agent>` with `--output-format json`: prints the answer, keeps agent + directory (or `--as NAME`) → session in `~/.kith/ask.json` so `-c` resumes it with the same project/model/effort; `KITH_VIA=ask` | you, scripts, Claude Code sessions |
| `bin/kith-remote` | python | Other machines over ssh: `add`/`ls`/`rm` (`~/.kith/remotes.json`); `ask AGENT@MACHINE` (from kith-ask; refused when `KITH_PERSON` isn't the owner): snapshot the working tree as a commit, push it to a scratch clone there, run `kith ask` there in a worktree, fetch the result branch and copy `.kith-out` back. Its agent side is `_repo` and `_run` | kith-ask, over ssh on the other machine |
| `bin/kith-memory` | python | Memory search: indexes the agents' memory and private skills and the state folder's user.md and projects/ in an in-memory FTS5 table per search, prints `path:line` + passage | you, agents (PROTOCOL.md; allowed without a prompt in `kith` runs) |
| `bin/kith-sched` | python | Scheduled jobs: cron parser, `jobs.json` per agent, `tick` starts due jobs as detached `kith <agent> "task"` runs (`KITH_VIA=schedule`, `KITH_PERSON` = the job's `person`, the one whose run added it; skipped when kith-auth no longer allows them), catches up once after sleep; launchd `dev.kith.sched` | you, launchd every minute |
| `bin/kith-state` | python | Commits what was lived: sqlite → text dumps, commit and push the state repo and the agents repo, then every role | you |
| `bin/kith-new` | bash | Scaffolds `agents/<name>/`: `role/` as its own repo, memory folders next to it | you, the builder agent |
| `bin/project-id` | python | Directory → project ID from `projects.json` (remotes and paths as aliases, a path also matching the folders inside it); `--info` the project's parents and homes; `--register` with `--in PARENT`, `--home [DIR]` (bare: a new folder inside the parent's home, else under the root); `--children ID [--all]` the projects it holds and where each is on this Mac (a repo is registered by remote and checkout); `--root [DIR]` where new homes go (`homes` in `projects.json`, default `~/Documents`, asked by `kith init`) | `bin/kith`, the adapter, agents |
| `bin/kith-project` | python | `kith project`: the project here and its parents (repos, home, memory, children), `ls` as a tree, `add` (→ `project-id --register`), `root` (→ `project-id --root`), `open [ID]`: Claude Code in the project's home with `--add-dir` for everything it holds | you |
| `bin/kith-acp` | python | `kith acp <agent> [--keep …]`: the agent as an ACP agent for ACP clients. Runs claude-agent-acp with `adapters/acp/claude-via-kith` as its claude; drops the client's MCP servers (except `--keep`) and `_meta.systemPrompt`; gives each group chat its own agent session (map in `~/.kith/acp/<agent>.json`), rewriting session ids both ways so the client only sees its own; logs in `~/.kith/logs/acp/` | an ACP client (a Kith bot's `command`) |
| `bin/kith-serve`, `bin/kith` | bash | `kith serve start\|stop\|restart\|status\|relay [URL]\|cloud`: builds `serve/host` (dev-app profile) when stale, only from `~/.kith/hq-serve` (another checkout hands over to it or refuses; `start --dev` / `KITH_SERVE_DEV=1` builds from the current one), installs `~/.kith/serve/bin/kith-serve`, runs its `install --port 19224` (launchd `dev.kith.serve`); relay URL in `~/.kith/serve/config.json`; `cloud [...]` forwards to the host's `cloud`, and `cloud --confirm-owner` (accept an owner change) is refused unless stdin is a terminal. `kith` forwards to `kith` | you |
| `bin/kith-send-to`, `services/grok.json` | python, JSON | `kith send-to <service> "text" [--body FILE\|-] [--profile P] [--for A] [--json]`: pushes to a bot's webhook (a service with a `webhook` block) through `kith-auth exec`; the URL and key reach curl on stdin only; 429 backs off; "sent" means received, not run | agents with the service in `needs:` |
| `bin/kith-send` | bash | Delivers a result: into the Discord thread when inside one, into the run's outbox (`KITH_BRIDGE_OUTBOX`, posted by the host) inside any other chat task, else Telegram | agents |
| `bin/kith-init` | python | Setup, safe to repeat: checks git (+ identity), claude, gh, uv; makes ~/.kith and the agents folder git repos with a first commit and `user.md`; offers private GitHub backups; installs what agents.json lists; offers a starter (`lib/starters.json`). `--from <user>` clones that user's hq-state and hq-agents first (a new Mac) | `install.sh` (last step), you |
| `bin/kith-doctor` | python | Read-only checks with a fix for each: folders and links, tools, the state, agents and role repos (uncommitted, unpushed, no backup), agent checks, grants, plugin version, the `plugins` every agent gets (and any not installed), each `dev.kith.*` job (loaded, running, stuck in kith-uv = lost Full Disk Access) | you |
| `lib/dot-kith.gitignore`, `lib/agents.gitignore`, `lib/user.md`, `lib/starters.json` | text | What `kith init` puts in a new ~/.kith and agents folder; the starter agents it offers (link null until published) | `kith-init` |
| `lib/kith_paths.py` | python | The resolver: KITH_HOME, state, agents (address and `_REAL`), machine, logs, caches; `--sh` / `--json` | every tool, the adapter |
| `lib/kith_settings.py` | python | kith's own settings (kith-wide, not secret): this Mac's `~/.kith/settings.json` over the defaults; today `permission_mode` (default `auto`), the mode a run starts in when neither the caller nor the agent sets one, `plugins`, plugin ids every agent's run turns on (the agent's own `enabledPlugins` wins), and `agents.<name>.permission_mode`, this Mac's choice over an agent's own (suggested) mode | `bin/kith`, `kith-doctor`, `kith-pkg` (install asks) |
| `adapters/claude-code/hooks/hooks.json` | JSON | Loads `register.tsx` as the plugin's hooks module | Claude Code |
| `adapters/claude-code/hooks/register.tsx` | TS | The Claude Code plugin: registers each agent as `kith:<name>` (pinned package, frontmatter `model`), puts project + memory in front of each spawn, applies `effort` per request, nudges for the session file, `/kith` and `mcp__kith__invoke` | Claude Code, every session |
| `adapters/acp/claude-via-kith`, `adapters/acp/shim/claude` | bash, python | The SDK's claude binary: saves the SDK's args and runs `kith <agent>` with the shim first on PATH; the shim merges kith's args with the SDK's (one appended system prompt, kith's first; the client's permission mode wins). The client's memory keeper runs on plain claude | `bin/kith-acp` (via claude-agent-acp) |
| `adapters/claude-code/.claude-plugin/plugin.json`, `.claude-plugin/marketplace.json` | JSON | Plugin manifest (option `hq_path`) and this repo as its marketplace | `install.sh` |
| `kernel/PROTOCOL.md` | md | The rules every agent gets after its `agent.md`: memory, projects, sessions, learned skills, delivery | `kith-pkg` (into `prompt.md`), the adapter |
| `kernel/services/<name>.json` | JSON | One shared service: kind (mcp, cli, login), how to start it, secret names (never values) | `kith-auth`, `kith-pkg` checks |
| `install.sh` (top) | bash | Links `~/kith` and `~/.local/bin/kith`, makes `~/.kith/secrets`, installs the plugin, then runs `kith init` with its arguments (`--from <user>`) | you, once per Mac |

## Placeholders and environment

Packages never hold absolute paths. `bin/kith` and the adapter fill these in at run time; `kith-pkg` rejects unknown ones.

| Placeholder | Becomes | Env var for tools |
|---|---|---|
| `{{KITH}}` | the kernel repo | `KITH_ROOT` |
| `{{AGENT}}` | the agent's name | `KITH_AGENT` |
| `{{AGENT_DIR}}` | the pinned snapshot this run uses | `KITH_AGENT_DIR` |
| `{{PACKAGE}}` | the agent's live `role/` (where learned skills are written) | |
| `{{HOME_DIR}}` / `{{HOME_REAL}}` | the agent's own folder `<agents>/<name>` (memory, data, files); the same with symlinks resolved, for permission rules | `KITH_AGENT_HOME` |
| `{{MEMORY_DIR}}` / `{{FILES_DIR}}` / `{{PERSON_FILE}}` | whose memory, outputs and profile the run uses: the owner's `memory/`, `files/`, `user.md`; anyone else's `memory/people/<id>/`, `files/people/<id>/`, `<state>/people/<id>.md` | |
| `{{SHARED_MEMORY}}` | `memory/shared/`: the craft, for everyone who uses the agent | |
| `{{STATE}}` / `{{AGENTS}}` | the state folder; the agents folder | `KITH_STATE`, `KITH_AGENTS` (also `KITH_HOME`) |
| `{{MACHINE}}` | this Mac's kith folder, `~/.kith` (…; never `secrets/`, which runs are denied) | `KITH_MACHINE` |
| `{{USER_HOME}}` | `$HOME` | |

Also set for a run: `KITH_AGENT_VERSION`, `KITH_MODEL`, `KITH_EFFORT`, `KITH_PROJECT` (read by `kith-cost tag`), and, from the
host's chat channels, `KITH_VIA=discord`, `telegram` or `slack`, `KITH_BRIDGE_OUTBOX`, `KITH_PERSON` for anyone but the owner (`schedule` from kith-sched, plus `KITH_SCHED_JOB`; `ask` from kith-ask), `KITH_DISCORD_WEBHOOK_URL`, `KITH_DISCORD_THREAD_ID` (read by `kith-send`), `KITH_CLAUDE_ARGS`
(extra `claude` flags; `bin/kith` clears it once read so helpers don't inherit it).

## Flows

**`kith <agent> "task"`** (terminal, or the host on a chat message)
1. `bin/kith` → `kith-pkg resolve <agent>`: unchanged package → its current snapshot; changed → check, snapshot into
   `~/.kith/store`, commit to the agent's repo; failing check → the last good version.
2. Prompt = snapshot `prompt.md` (agent.md body + PROTOCOL.md) with placeholders filled + `MEMORY.md` + session tag.
3. Settings = snapshot `settings.json` + a SessionStart hook → `kith-cost tag`; installed plugins off but `kith@kith` and
   `plugins` from `~/.kith/settings.json`, unless the agent's `enabledPlugins` sets them. MCP = `kith-auth mcp-config <agent>` +
   the package's `mcp.json`. `--model`/`--effort` from frontmatter.
4. `exec claude`. The agent reads skills from `{{AGENT_DIR}}`, writes memory to `{{HOME_DIR}}`, delivers with `kith-send`.

**`kith:<name>` inside any Claude Code session** (`register.tsx`)
1. Session start: `kith-pkg resolve --all --json` pins every agent for the session; each is registered with its prompt
   and model; efforts are noted.
2. Spawn: project from `<kith project>` or `project-id`, its parents (`project-id --info`), plus fresh `MEMORY.md`, go in front of the task. Every session in a project with a parent, home or children gets one `kith project:` line in its system prompt (`prompt.compose`).
3. Each request: the agent's effort. Stop: one nudge if the session file wasn't written.
   Runs on the caller's settings and MCP servers, not the agent's. A kith service's MCP tool the agent itself isn't
   granted (`kith-auth helper-mcp`, a PreToolUse hook) is asked in the owner's session and denied in anyone else's.

**An agent changes** (you edit it, ask it to, or it writes a learned skill into `{{PACKAGE}}`)
→ the next run's fingerprint differs → `kith-pkg` checks it → snapshot + commit to `agents/<name>` →
`kith push <name>` or `kith-state commit` pushes it. Running sessions keep their snapshot.

**What was lived changes** (memory, data) → written straight to `agents/<name>/` → `kith-state commit` dumps the
databases and commits and pushes the agents repo and the state repo.

**`kith ask house@mini` from another machine** → kith-ask hands it to kith-remote → in a git repo: working tree → commit
(temp index) → `git push` over ssh to `~/.kith/cache/handoff/<repo>/repo` on mini → ssh `kith remote _run`: worktree on
`kith/house/<id>`, `kith ask house` there (mode from its `KITH_REMOTE_PERMISSION_MODE`, else the agent's, else kith's default;
`CLAUDE_CODE_OAUTH_TOKEN` from its env file), leftovers committed →
JSON answer back → `git fetch` the branch, `scp` `.kith-out/` to `./kith-out/<id>/`.

**A scheduled job** → launchd runs `kith-sched tick` every minute → for each `jobs.json` job whose cron time fell since
the last tick (at most a day back) and that isn't still running: `kith <agent> "task + scheduled note"` detached, with
`--permission-mode` from `KITH_SCHED_PERMISSION_MODE` when set (else the agent's mode, else kith's default) → output in
`~/.kith/logs/sched/runs/<agent>/`, delivery by `kith-send`.

**Cost** → Claude Code writes transcripts → `kith-cost sync` (every 6 h, and before each report) → `cost.db`;
`runs` (from the hook) and `kith:` subagent types attribute requests to agents.

## Where to change what

| To change | Open |
|---|---|
| What every agent is told (memory, sessions, delivery rules) | `kernel/PROTOCOL.md` |
| How a run is assembled (flags, prompt, settings, model/effort) | `bin/kith`, and the same in `register.tsx` for subagents |
| What makes a version valid, snapshots, auto-commit, install/share | `bin/kith-pkg` (`problems()` for checks) |
| A new placeholder | `bin/kith` `fill()`, `register.tsx` `fill`, `kith-pkg` `PLACEHOLDERS` |
| Services, grants, secrets | `kernel/services/`, `kernel/bin/kith-auth` |
| Chat behaviour (Discord, Telegram, Slack) | `serve/host/src/channels/`; delivery in `bin/kith-send` |
| How memory is searched (what's indexed, ranking) | `bin/kith-memory` |
| Scheduled jobs (cron syntax, catch-up, how a job runs) | `bin/kith-sched` |
| Where kith's folders are (KITH_HOME, ~/.kith and what is in it) | `lib/kith_paths.py`, nowhere else |
| An kith-wide setting that isn't secret (the default permission mode) | `lib/kith_settings.py` `DEFAULTS`; per Mac, `~/.kith/settings.json` |
| A background job | its row in [Background jobs](#background-jobs): the installer, or the plist itself where there is none |
| Calling agents on other machines, the code handoff | `bin/kith-remote` (`call` on the caller, `serve_run` on the agent's machine) |
| Cost reports or prices | `bin/kith-cost` (`PRICES`) |
| The scaffold for new agents | `bin/kith-new`, and the builder agent's `new-agent` skill (in its own repo) |
| One agent's behaviour, skills or tools | that agent's repo, `agents/<name>/` (its own repo; the kernel ignores it) |
