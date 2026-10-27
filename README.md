# kith

**Want a personal agent like Grok bots, Muse or Dots, but on your own computer, working with your files?**<br>
**Want each agent kept separate, with its own skills, tools, memory and accounts?**<br>
**Want one place to manage them all, with a history you can roll back?**<br>
**Rather start from an agent someone already built than from scratch?**

That's what kith is built for.

Persistent specialist agents you can call from any repo. Each has its own skills and layered memory, and each is its
own git repo: install one from a GitHub link, change it, roll it back, share it or keep it private. Persistent means
durable identity and memory, not a long-running process: every call is a fresh session that picks up where the last
one left off.

## What makes it different
- **An agent is a folder on your computer, not a container or a hosted service.** A personal agent's abilities come
  from its machine: your browser profile, Keychain, GPU, local models, the CLIs you're logged in to. kith runs agents
  where those are, as plain Claude Code sessions, instead of rebuilding them somewhere else.
- **An agent is a git repo.** Install one from a GitHub link, change it however you like, and every version that
  passes its checks is committed. Roll back to any of them, share one, or keep it private. The agent learning a new
  skill is a change like any other: versioned, and undone the same way.
- **What it is and what it has lived are kept apart.** Its role (instructions, skills, tools) is shareable; its
  memory, data and files sit next to it, in your private agents repo. Sharing an agent never shares your life, and upgrading one never
  touches its memory.
- **Agents carry no accounts.** An agent says which services it needs; you grant it one of your accounts. Secrets reach only the
  process that uses them. Two people can run the same agent with their own logins.
- **Specialists stay cheap.** Each capability belongs to one agent and loads only when that agent runs, so your main
  session sees one line per agent, and adding agents doesn't make every session heavier.

## Core idea: simple, stable, scalable
- **Simple.** An agent is markdown plus a few tools in a folder. The kernel is a handful of scripts (bash and Python
  standard library) on top of Claude Code and git: no server, database, daemon or container. If you can read a
  folder, you can read an agent.
- **Stable.** Every run is pinned to a read-only snapshot, so an agent can change mid-session without breaking a run
  in progress. A version that fails its checks never runs (the last good one does), every good version is in git,
  and a missing secret or broken service costs a run that service, never the run itself.
- **Scalable.** Adding an agent is adding a repo; nothing central grows. Because each agent loads only its own
  skills, context stays flat as you add more. Agents travel by GitHub link and are found by topic, so going from
  one person's agents to everyone's is sharing a repo, and serving many people is one machine per person running kith,
  not a platform.

How it fits together and where it's going: [docs/architecture.md](docs/architecture.md); one run, drawn:
[docs/how-hq-runs.html](docs/how-hq-runs.html); every file and which one to open for a change:
[docs/codebase-map.md](docs/codebase-map.md).

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/img/how-hq-runs-dark.png">
  <img alt="How an kith agent runs: entry points, kith-pkg resolve, snapshot, bin/kith, Claude Code run; kith-auth with shared services and auth.json; MCP server" src="docs/img/how-hq-runs-light.png">
</picture>

```
curl -fsSL https://raw.githubusercontent.com/Ying-Kai-Liao/kith/main/scripts/install.sh | sh   # a release: no git or Rust needed
git clone https://github.com/Ying-Kai-Liao/kith ~/kith && ~/kith/install.sh   # or from source; ~/kith: any folder you like
kith builder                           # meet the starter: it learns who you are, then builds your agents with you
kith install <owner>/<repo>            # an agent someone shared: any GitHub repo with an agent.md at its root
kith new reading "tracks what I read"  # or start one yourself
kith doctor                            # check everything, with what to fix
kith backup                            # everything to private GitHub repos: state, memories, your agents
```
Kith was called hq until 2026-10-22. The `hq` command (and `hq-<tool>`) still works as an alias, a Mac with an existing `~/.hq`
keeps using it until `kith migrate` moves it to `~/.kith`, and `~/hq` stays a link to the same folder as `~/kith`.
Old `HQ_*` variables, `dev.hq.*` jobs and `hq-agent` repos are still read.

The download installs the `kith` binary and the kernel under `~/.kith`, then runs the same `install.sh`; run it again to
upgrade. `KITH_VERSION`, `KITH_PREFIX` and `KITH_INSTALL_TARBALL` (a local file) are in the script's header. Homebrew:
`packaging/homebrew/kith.rb` (for the tap [`Ying-Kai-Liao/homebrew-kith`](https://github.com/Ying-Kai-Liao/homebrew-kith)). Releases are built by `.github/workflows/release.yml`
on a `v*` tag.

The Mac app (`apps/`, a dmg) carries all of this inside: the host (`Contents/Resources/kith-serve`, run as a launchd job)
and the release layout (`Contents/Resources/kith/bin/kith` with its kernel), so it needs no separate install; see
[apps/README.md](apps/README.md).

A Linux VM that only needs to join a computer and ask its agents (a cloud bot) installs the static client-only `kith`, no root:
`curl -fsSL https://raw.githubusercontent.com/Ying-Kai-Liao/kith/main/scripts/install-guest.sh | sh`, then `kith join <link>`
(x86_64 and aarch64; `KITH_VERSION`, `KITH_GUEST_PREFIX` in the script's header).

`kith` is one binary: `kith serve <...>` is the host, `kith --version`, and every other `kith <args>` runs the kernel's
`kith <args>`. It finds the kernel in `$KITH_KERNEL`, else `share/kith/kernel` beside the binary (the download, Homebrew),
else `~/kith/kernel` (a git checkout).

This repo is the kernel only: no agents ship with it. `install.sh` links the `kith` CLI, installs the Claude Code
adapter plugin (`adapters/claude-code`, served by this repo's own marketplace), then runs `kith init`: it checks what
kith needs (git and its identity, Claude Code, gh), makes `~/.kith` and the agents folder (each a git repo), a
`user.md` for your agents to fill in, asks once whether to back everything up to private GitHub repos (off by
default: it stays on this Mac, which Time Machine backs up; `kith backup` later), and offers the builder, the
agent that builds the others, then lists the agents people shared (GitHub repos tagged `kith-agent`) to install
any by number; `kith market` shows that list any time. Run `kith init` again any time; it only does what is missing.
On a new Mac, `install.sh` finds your backup through your GitHub login and offers to bring it back. Then, in any Claude Code session:
"have the designer critique this page", `/kith branding <task>`, or the `mcp__kith__invoke` tool. Agents appear as
`kith:<name>`; your main session only sees one line per specialist, and their skills load only when they're called. Or
run one directly in a terminal: `kith branding`.

The plugin reads the installed agents at session start: edit an `agent.md` or `PROTOCOL.md` and the next session has it.
On every spawn it resolves the project, puts it and the agent's long-term memory in front of the task, and when the
agent stops without writing its session file, asks it to once.

## Agents
Each agent is a folder in the **agents folder** (`agents/` in the kith folder): its role is a git
repo in `agents/<name>/role/`, its life (memory, data, files) is next to it.
Whoever changes it (you, a code agent, or the agent itself writing a learned skill) changes that checkout; every
version that passes its checks is committed there automatically, so its history is the agent's history.
```
kith install <owner>/<repo>                      # or a github.com URL; .../tree/<ref>/<folder> for one agent in a bigger repo
kith install <link> --as <name>                  # under another name
kith install <link> --yes                        # without the question (the summary is always printed)
kith update <agent>|--all                        # pull upstream (fast-forward only; your edits are never overwritten)
kith history <agent>                             # every committed version
kith restore <agent> <commit>                    # bring one back, as a new commit (undo with another restore)
kith share <agent> [--public]                    # put it on GitHub as <you>/kith-agent-<name>, private unless --public
kith share <agent> --public --fresh              # a public copy without its history, at <you>/kith-<name>
kith market [words]                              # agents people shared, most starred first: pick any by number to install
kith search [words] [--mine]                     # find agents: GitHub repos tagged kith-agent (yours incl. private, anyone's public)
kith deps <agent>|--all [--install]              # programs an agent runs that this Mac lacks; --install adds them
kith push <agent>|--all                          # push committed versions (kith-state commit does this for every agent)
```
Installing shows what the agent would do on your Mac before it is kept: its permission mode, what it may run
without asking, hooks, MCP servers, tools, skills, and the services it asks for (nothing is shared until you grant
it). An agent may bring the definition of a service the kernel lacks in its own `services/<name>.json`.
Programs it runs go in `requires:` in its `capabilities.yaml` (`ffmpeg: brew install ffmpeg  # cutting footage`):
installing lists the missing ones and offers to install them, `kith doctor` keeps checking, `kith deps <agent> --install`
adds them later.
`--public` runs the privacy check over the package *and its whole history* and refuses while either names you: a
public repo shows every old commit. `--fresh` publishes a separate public copy instead: one commit per publish
with the agent as it is now (its files still checked), while the agent's own repo keeps every version and stays
private. Every share writes a README from `agent.md` unless the agent has one. An agent installed from someone else keeps their repo as `upstream`; `kith share`
makes yours `origin`, so `kith update` still pulls their changes.

## Repos, and where each change goes
Agent repos carry the GitHub topic `kith-agent` (`kith share` adds it) and the kernel `kith`, so they group without an
organization: `kith search --mine` lists yours, private ones included, and public agents from anyone show up in
`kith search` and at github.com/topics/kith-agent. To publish an agent of your own made outside `kith share`, add the topic.

Running kith takes **3 + N repos**: this kernel, your state repo, your agents repo, and one per agent role you've shared. Agents installed
from someone else stay in their repo (+0 until you `kith share` your own copy); third-party skills are fetched from
their authors' repos (+0). Several agents can share one repo, installed by folder (`kith install
<owner>/<repo>/tree/main/<folder>`), though `kith share` makes one repo per agent.

| Repo | Holds | Visibility |
|---|---|---|
| `kith` (this one) | the kernel: `kernel/` (code, `PROTOCOL.md`, `services/`, the plugin), docs, `install.sh` | public |
| `<you>/hq-state`, cloned to `~/.kith` | kith's knowledge: `user.md`, projects, `agents.json`, `private-words` | **private** |
| `<you>/hq-agents`, cloned to `agents/` | each agent's life: memory, data (as text dumps), jobs, private skills | **private** |
| `<you>/kith-agent-<name>`, in `agents/<name>/role` | one agent's role: `agent.md`, settings, skills, tools, learned skills | private unless you share it |

| You change | It goes to | Saved by |
|---|---|---|
| An agent (you edit it, ask it to change, or it learns a skill) | that agent's repo | committed automatically on its next run; pushed by `kith-state commit` or `kith push <name>` |
| A new agent (`kith-new`, or ask the builder) | a new local repo in the agents folder | `kith backup` once creates its private GitHub repo; from then on as above |
| The kernel or `PROTOCOL.md` | this repo | a normal commit and push; it's public, so `kith-pkg check --share` first |
| A service (`kernel/services/<name>.json`) | this repo | as the kernel; it names its secrets, never their values |
| Memory and data | the agents repo | `kith-state commit` |
| Your profile, projects, which agents you have | the state repo | `kith-state commit` |

`kith backup` does all of it at once: a private repo for each piece that has none yet (it lists them and asks), then
commits and pushes everything. Saying yes to backups in `kith init` also runs it every night at 03:00 (`kith backup
--nightly`, a LaunchAgent; `--nightly off` stops it), so a new agent of yours gets its private repo the next night.
`kith doctor` says in one line what is backed up, what changed since, and what is
only on this Mac.

A kernel change reaches every agent on its next run without touching their repos. An agent that starts to `need:` a
new service only passes its checks where that service exists, so push the service with the kernel.

**On a new Mac**, install as usual: `kith init` asks whether to bring back a GitHub backup (logging you in to
GitHub if needed), finds `<you>/hq-state`, fetches it into `~/.kith` and your hq-agents into `agents/`, then installs
each agent's role (agents.json) into `agents/<name>/role`. It names any agent that was never backed up.
`install.sh --from <you>` does the same without asking.

**Not in any repo**, so redo them by hand on a new Mac:
- an agent you never shared: `agents.json` records it without a link, so `install.sh` skips it;
- grants and profiles (`~/.kith/secrets/auth.json`), secrets (Keychain), `~/.kith/secrets/env`: `kith auth grant`, `kith auth set`;
- LaunchAgents (`~/Library/LaunchAgents/dev.kith.*.plist`): `kith schedule --install`, `kith-bridge --install`, each
  tool's own `install` command. The jobs themselves are in the agents repo.
- what agents made (`agents/<name>/files/`): on disk only; back it up with the rest of the Mac.

## Layout
Everything is in the kith folder, laid out by what each thing is (docs/architecture.md, Concepts): kith and each agent
have what they **are** (shareable, versioned) and what they **have lived** (private). The kith folder is this repo; its
code is in `kernel/`, which is what kith is. Each agent is a folder in `agents/`: `role/` is what it is (its own repo),
and next to it what it has lived: `memory/`, `data/`, `files/`. This repo ignores `agents/` (its own private repo),
so `git pull` only ever updates kith. Don't run `git clean -x` here: it deletes ignored folders. `~/kith` is kith's stable
address, a link to the kith folder (made by `install.sh`), so the folder can live anywhere, an external disk included;
`KITH_HOME` points somewhere else instead.

`~/.kith` is this Mac's kith, a real folder on the internal disk, as `~/.claude` is Claude Code's. What kith has lived (who
it works for, projects, the team, rules) is its git repo (your hq-state); its `.gitignore` commits only those files,
so everything else in it stays on this Mac: `secrets/` (grants, tokens; agents' file tools are denied it), settings,
job state, the cost ledger, role snapshots, `logs/`, `cache/`.
```
install.sh                 links ~/kith and ~/.local/bin/kith, makes ~/.kith, installs the Claude Code plugin
.claude-plugin/            this repo as a plugin marketplace (serves kernel/adapters/claude-code)
docs/                      architecture, codebase map
kernel/                    the code
  adapters/claude-code/    the Claude Code adapter (plugin)
  adapters/harness.py      the other harnesses an agent can run on: codex, pi, agy, hermes, qwen, dsh, opencode
  lib/kith_paths.py          the one place that knows where things are
  bin/project-id           resolves the current directory to a project; --list, --register
  bin/kith                   run an agent directly: kith ls · kith <agent> [-p ID] [task] · --print
  bin/kith-pkg               agents: install, update, history, restore, share; versions: resolve (pin a run), pin, check
  bin/kith-new               scaffold a new agent as its own repo
  bin/kith-auth              shared services: which agent may use which, as which account
  bin/kith-state             commit what was lived: state and agents repos (sqlite dumped as text), then each role
  bin/kith-ask               ask an agent and get its answer back; continue the conversation (-c)
  bin/kith-live              live agents (session: live): start, find and message their one session
  bin/kith-remote            agents on your other machines over ssh: kith remote add, kith ask agent@machine
  bin/kith-memory            search the agents' memory and shared notes
  bin/kith-sched             scheduled agent jobs (cron, launchd)
  bin/kith-account           extra Claude / Codex logins and the backup chain: kith account add, kith backup list|add|remove
  PROTOCOL.md              memory rules, appended to every agent's prompt
  services/<name>.json     shared services (MCP servers, APIs, logins): how to start them, which secrets by name

agents/                    the agents folder (ignored by this repo; your hq-agents repo)
  <name>/                  one agent
    role/                  what it is: its own git repo (shareable)
      agent.md               identity, how it works, skill index
      capabilities.yaml      tools and data sources it may use, by name only — never credentials
      settings.json          Claude Code settings merged over your global ones for kith runs: model, plugins, permissions
      skills/ tools/         its skills (read on demand) and the tools they drive
      skills/learned/        skills it wrote itself; versioned and shared with it
    memory/MEMORY.md       long-term index (auto-loaded on every call)
    memory/projects/<id>.md   its notes on one project
    memory/sessions/<id>.md   where the last call on that project left off
    data/                  its databases; committed as text dumps (<name>.sql), not binaries
    files/<project>/       what it made: renders, images, exports, backups (on disk, not in git)
    skills/learned/        private skills, never shared (kith-pkg promote <agent> <skill> moves one into role/)
    jobs.json              its scheduled jobs (kith schedule)

~/.kith/                     this Mac's kith; what kith has lived is your kith-state repo
  projects.json            project IDs → the git remotes / paths that belong to them
  agents.json              which agents you have and where their roles come from
  private-words            words no shared role may contain (kith-pkg check --share)
  user.md                  who kith works for (incl. visual taste), read by every agent
  projects/<id>/           codebase.md (shared project map) + verify.md (how to check a change) + decisions.md (append-only log)
  secrets/ … store/ logs/  this Mac only, never committed (snapshots of roles: store/<name>@<version>/)
```

## Versions
An agent's package is its `role/`; its life is the folder around it. Every run is pinned to a snapshot of
it, so agents can be upgraded at any time, by anyone (you, the agent writing a learned skill, a code agent like this
one), without waiting for running sessions:

- On each run `bin/kith-pkg` fingerprints the package. Unchanged: run the existing snapshot. Changed: check it (frontmatter,
  JSON, skills named in agent.md exist, needed services exist), then snapshot it into `~/.kith/store/<name>@<version>/`
  (an APFS clone on the same disk, else hard links to the previous one's unchanged files), and commit it to the agent's repo. Prompt and settings stay templates in the snapshot; placeholders
  are filled at run time. Running sessions keep the snapshot they started on.
- A version that fails its checks is never used; the last good one runs and the reason is printed. Checks:
  frontmatter, JSON, unknown `{{PLACEHOLDERS}}`, skills named in agent.md exist, needed services exist.
- Multi-file edits: `touch <agents folder>/<name>/.editing` first and delete it when done; runs keep the last good version
  meanwhile, so nobody gets a half-edited agent. A marker older than an hour is ignored.
- If versioning itself fails, the run uses the live folder and says so, rather than not running.
- `kith versions <name>` lists them; `kith rollback <name>` pins the previous one; `kith pin <name> <version>`;
  `kith unpin <name>` follows edits again. Old snapshots are removed after two days unless pinned, good or running;
  the commits stay, and `kith restore` brings any of them back.
- `kith:<name>` subagents are pinned per Claude Code session (the adapter resolves every agent at session start).

In prompts and settings: `{{AGENT_DIR}}` is the pinned package (read skills and run tools from it),
`{{PACKAGE}}` the agent's live `role/` (where it writes learned skills), `{{AGENTS}}` the agents folder (another
agent's tools: `{{AGENTS}}/house/role/tools/house.py`), `{{HOME_DIR}}` the agent's own folder (memory, data, files), `{{HOME_REAL}}` the same with
symlinks resolved (use it in permission rules: Claude Code matches resolved paths), `{{STATE}}` the state folder,
`{{HQ}}` the kernel repo, `{{USER_HOME}}` your home folder (for permission rules, instead of `/Users/<name>`). Tools find their data through the snapshot's `.home` file; run from the live `role/` (launchd,
another agent), they use the folder around it.

## Third-party skills
Skills from public GitHub repos are linked, not copied. An agent's `skills.json` names each one's `repo`, `path`
and `ref` (a full commit sha) and its licence; `kith-pkg fetch <agent>|--all` downloads just those folders into
`skills/<name>/` (a shallow, sparse fetch: seconds, however big the repo), where they're git-ignored and carry a
`.source` file. `install.sh` fetches every agent's; a run fetches anything missing on first use. A package whose
linked skills aren't fetched fails its checks, so the last good version runs. To upgrade one, change its `ref`.
Notes of your own about a linked skill go next to it (image's `skills/anatomy-kith.md`), never inside it. A skill
for your own install only (one you can't or won't share) goes in the agent's own `skills/learned/` (next to `role/`, not in it),
not the package, listed in its MEMORY.md.

## Services and auth
Three layers, so sharing an agent never shares an account:

| Layer | Where | Says |
|---|---|---|
| Service | `kernel/services/<name>.json` (repo) | what it is (`mcp`, `cli`, `login`), how to start it, which env vars it needs |
| Need | `needs: [...]` in the agent's `capabilities.yaml` (package) | what the agent asks for |
| Grant | `~/.kith/secrets/auth.json` (yours, never in git) | profiles (an account: where each secret comes from) and grants (agent → service → profile) |

```
kith auth                                   # who needs what, who has what, whether each secret resolves
kith auth grant personal notion notion/library
kith auth set notion/library NOTION_API_KEY # store in Keychain (prompts), point the profile at it
```
`bin/kith` starts only the MCP services an agent needs *and* is granted; secrets are resolved when the server starts
(`kith-auth exec`) and exist only in that process, never in a config file or the agent's environment. A cli tool asks
`kith-auth get --for <agent>`, which refuses agents without a grant. Every secret handed out is logged (time,
service, profile, who asked, who was running; never the value) to `~/.kith/secrets/auth.log`. A broken `auth.json`
or a secret that doesn't resolve costs the agent that service for the run, never the run itself. Profile sources: `keychain:SVC#ACCOUNT`,
`env-file:PATH#VAR`, `env:VAR`. This keeps cooperating agents to least privilege; it isn't a sandbox (an agent with
a shell can read what your user can). `kith:<name>` subagents run inside the calling session and share its MCP
servers; give an agent with its own credentials a full `kith <name>` run when it needs them.

## Settings
`kith <agent>` runs a normal Claude Code session: your global settings, hooks, skills and the repo's own config all
load. The agent's `settings.json` (rendered into the snapshot) is merged on top to narrow it per agent: all agents turn off Claude Code's own auto
memory (kith memory replaces it) and the flow plugin (flow calls agents, not the other way round); designer links impeccable as a
pinned skill, devops keeps vercel; finance and personal can't edit `~/.hermes`. Subagent calls (`kith:<name>` inside a
session) use that session's settings instead.

**Plugins**: a run turns on the kith plugin and whatever the agent's `settings.json` enables; every other installed plugin
is off. For a plugin every agent on a Mac should load, list it in that Mac's `~/.kith/settings.json`:
`{"plugins": ["jev-effort-router@jev-effort-router"]}` (ids as `claude plugin list` shows them). An agent whose
`settings.json` sets that plugin in `enabledPlugins` keeps its own choice. `kith <agent> --inspect` shows what a run loads.

**Model and effort** live in `agent.md`'s frontmatter (`model: sonnet`, `effort: low|medium|high|xhigh|max`), so they
apply to both `kith <agent>` runs and `kith:<name>` subagents and change with the agent's version. Override one run with
`kith <agent> -m MODEL -e EFFORT` (or `KITH_MODEL` / `KITH_EFFORT`); leave a field out to use your global setting.

**Harness.** An agent runs on Claude Code unless `agent.md` says `harness:` with one of `codex` (OpenAI Codex CLI),
`pi`, `agy` (Antigravity CLI), `hermes` (Hermes Agent), `qwen` (Qwen Code), `dsh` (DeepSeek Harness) or `opencode`.
`kith <agent> -H codex`, `kith ask <agent> -H pi`, `kith schedule add … -H hermes` or `KITH_HARNESS` pick one for a run; the
Discord bridge uses the agent's own. Every harness gets the same prompt (agent.md, PROTOCOL, memory, project),
directories, model and effort; `kernel/adapters/harness.py` turns them into its flags and its answer into the shape
`kith ask` and the bridge read, with sessions you continue (`-c`, a thread) on the same harness. A `model:` names a model
of the agent's own harness, so with `-H` the other harness uses its default unless `-m` says. What only Claude Code
runs have: the agent's `settings.json` (permissions, hooks), per-request costs (other harnesses: one ledger row per
run), and on agy, hermes, qwen and dsh, the granted MCP servers (codex, pi and opencode get them). dsh takes its
DeepSeek key from its own settings or a `deepseek` grant (`kith auth`); dsh and opencode run through `npx` when not
installed. `kith:<name>` subagents always run inside the calling Claude Code session.

**Backups.** When an agent's own harness runs out of usage, `kith serve` can move the turn to a backup, in the order you set:
`kith backup add claude:work` (an extra Claude subscription login made with `kith account add claude work`, kept in
`~/.kith/accounts/claude/work` through `CLAUDE_CONFIG_DIR`), `claude-api:<profile>` (an Anthropic API key held by `kith auth`,
service `anthropic`; Claude Code fetches it through `apiKeyHelper`, so it is never in env or logs), `codex` or
`codex:<name>` (`kith account add codex <name>`). The list is empty until you add to it; `kith backup … --agent A`
sets it for one agent (an explicit empty list means none). Anthropic's usage policy forbids using several accounts to get past
usage limits; `kith account add claude` says so and asks first. `kith <agent> --backups-json` lists the chain with each
entry's readiness and `kith <agent> --launch-json --backup <id>` launches one (exit 4 when it can't run); both are for
programs. The ledger records the harness and login of each run (`provider`, `account`).

## Cost
`bin/kith-cost` keeps every Claude Code request's cost (list API prices) in `~/.kith/cost.db`, so it outlives
transcript cleanup. Each session `kith` starts is tagged with the agent, package version, model, effort and caller
(terminal or discord); `kith:<name>` subagents are recognised by their type. `kith cost agents [today|week|month|all]`
compares agents; `kith cost agent video` shows one agent by day, by version/model/effort and its costliest runs;
`kith-cost csv` exports every request with its agent for analysis elsewhere.

## Asking an agent from a script or another session
`kith ask house "what expires this week?"` runs the agent exactly as `kith house "…"` does (its own package, settings,
services, memory, project) but prints only its answer and keeps the session: `kith ask house -c "and next week?"`
continues the same conversation from the same directory, `--as NAME` names one to continue from anywhere, and a
continued conversation keeps its model, effort and harness (`-H`). `--json` gives answer, session and cost for programs; `kith ask ls`
lists the conversations. A Claude Code session can use it to work with a specialist over several turns in the
specialist's own tools, where an `kith:<name>` subagent would run in the caller's. Tagged `via=ask` in the cost ledger.

## Live agents
An agent with `session: live` in its agent.md (coder) runs as one long-lived session that takes jobs one at a time:
`kith live start coder` starts it in its home folder, in an Orca tab, a tmux session (`kith-coder`) or this terminal.
`kith live where coder` finds it from its own process, however it was started. While it runs in Orca or tmux, the
owner's chat messages to it and `kith ask coder "…"` are typed into it (`kith-live send`, a header the kernel writes
with `reply=`) instead of starting a run; `kith ask` returns at once with an id and `kith ask --wait <id>` prints the
answers, which the agent sends with `kith-send --to <reply>`. It is never an `kith:<name>` subagent, resource-manager
never closes it, and anyone but the owner is refused. When it isn't running, a message runs it once as usual.
`kith live login on coder` starts it whenever you log in (LaunchAgent dev.kith.live, through kith-uv like the other jobs):
it waits for Orca, else uses tmux, and leaves a session that already runs alone (`agents.coder.live_at_login`
in ~/.kith/settings.json; `kith init` asks).

## Agents on your other machines
An agent lives on the machine that has its tools. On a laptop, `install.sh` asks whether your agents run on another
Mac and sets this up: it finds your Macs on Tailscale, makes an ssh key, asks for that Mac's password once, and adds
it. By hand: `kith remote add mini you@mac-mini`
(anything you can `ssh` into without a password: Tailscale, a LAN) and then `kith ask house@mini "…"`: the agent runs
there, with that machine's memory and accounts, and only the task, your code and its answer travel; `-c` continues.
Asked from inside a git repo, it gets your working tree (uncommitted and untracked files too) in a scratch worktree
on its machine, and whatever it changes comes back as a branch, `kith/<agent>/<id>`, with the one command to review it
and the one to apply it. Files it makes land in `./kith-out/<id>/`. For your own machines only: it's plain ssh, so it
adds no server, port or token. Once on the agent's machine: `claude setup-token`, and the token in
`~/.kith/secrets/env` as `CLAUDE_CODE_OAUTH_TOKEN`, because macOS keeps the keychain (Claude Code's login) from ssh
sessions. Details: `kith remote --help`.

## Schedules
`kith schedule add house "0 8 * * *" "Check what expires this week and send the list"` gives an agent a job: at that
time `bin/kith-sched` runs the same one-shot `kith house "…"` you would, tagged `via=schedule` in the cost ledger, and the
agent delivers with `kith-send`. Jobs are kept with the agent's life (`agents/<name>/jobs.json`), so they are
private, versioned with your memory and never part of a shared package. `kith schedule` lists jobs with their next run,
`kith schedule log house <id>` shows the last run's output, `pause`/`resume`/`rm`/`run` do what they say. Run
`kith schedule --install` once per Mac: a LaunchAgent ticks every minute, and a time missed while the Mac slept runs
once on the next tick. Nobody approves tool calls in a scheduled run: it starts in `KITH_SCHED_PERMISSION_MODE` when
set, else in the agent's own mode, else in kith's default (`auto`, as every other run), and the agent's own allow/deny
rules still apply.

## Permission mode
Every run starts in the first permission mode that is set: the caller's (`kith <agent> --permission-mode MODE`, or
`--bypass` for `bypassPermissions`; else `KITH_PERMISSION_MODE`: the bridge, schedules,
`kith ask` and remote runs pass theirs from `KITH_BRIDGE_`, `KITH_SCHED_`, `KITH_ASK_` and `KITH_REMOTE_PERMISSION_MODE`), your
choice for that agent, the agent's `settings.json` `permissions.defaultMode`, then kith's default, `auto`. To change kith's
default on a Mac, put `{"permission_mode": "acceptEdits"}` (or another Claude Code mode) in its `~/.kith/settings.json`,
kith's own settings: not secret and never committed. `kith doctor` shows the default in force. People other than you get
their membership's mode (`kith auth member`).

An agent's `defaultMode` is its author's suggestion. Installing someone's agent shows it and asks whether to keep it
(`bypassPermissions` only on a yes), and saves your answer in the same file, so an update of theirs can't change it:
`{"agents": {"<agent>": {"permission_mode": "auto"}}}`. Edit that line to change it later.

## Memory layers
1. **Identity and skills:** `agent.md` + `skills/` (including the ones it learned) + `capabilities.yaml`.
2. **Long-term:** `memory/MEMORY.md` and topic files: the craft, lessons. The user lives in
   the state folder's `user.md`.
3. **Project:** the state folder's `projects/<id>/` (shared by all agents) + `memory/projects/<id>.md` (private). The codebase map
   records its commit, so agents only re-read what changed since.
4. **Session:** the live conversation, plus `memory/sessions/<id>.md` across calls.

`kith memory search <words>` searches it all (every agent's memory and learned skills, `user.md` and `projects/`) and prints
`path:line` with the passage; agents use it for anything their index doesn't cover. It indexes the files afresh on
each search, so the markdown stays the only copy.

Memory is the agent's perspective, not the truth: code, specs and live data win. Current facts (expenses, calendar,
infrastructure) are read from their source every time, never from memory.

## Projects
A project is a named thing an agent builds a relationship with; git remotes and paths are aliases that point to it
(`projects.json` in the state folder). One project can span several repos and survive renames: `acme` (formerly Widget) covers both
`acme-web` and `widget-app`. Remotes work on every device; paths are machine-specific.

How an agent finds its project, first match wins:
1. **Given:** `kith <agent> -p acme`, or the project named in the request.
2. **Matched:** `bin/project-id` finds the current directory's remote or path in `projects.json`.
3. **Default:** non-code agents use a fixed ID outside a repo (`finance`, `personal-places`, …).
4. **None:** no project memory. Nothing is registered automatically; the agent asks you for a name first.

```
kernel/bin/project-id             # ID for the current directory, or nothing
kernel/bin/project-id --list      # all projects and their aliases
kernel/bin/project-id --register acme [dir]   # add dir's remote (or path) to a project
```

## Adding an agent
`bin/kith-new <name> "<description>"`, or ask the builder. It creates `agents/<name>/` with its role as its own git
repo in `role/` (`agent.md`, `capabilities.yaml` with `needs:`, `settings.json`) and its memory folders next to it;
put its skills in `role/skills/`. New sessions pick it up; `kith share <name>` puts it on GitHub.

## Notes
- The state folder holds personal data: keep its remote **private**. `kith-state commit` commits and pushes it, then
  pushes every agent's repo.
- Packages carry no personal details: those go in the state folder. `kith-pkg check --share` flags home paths, email
  addresses and the words in `~/.kith/private-words`.
- Status: early, built for one person's Mac. macOS only (APFS clones, Keychain, launchd); needs Claude Code, git
  and python3 (gh to share agents, uv for the Discord bridge).
- Licensing: see [Licensing](#licensing). Linked skills keep their own licences (each agent's skills.json).
- Design working files live outside the repo in `~/Workspace/design/<project-id>/`; sync that folder separately.

## Licensing

Kith is split on purpose. The pieces you build on are open source; the product around them is Fair Source.

| Part | Folders | License |
| --- | --- | --- |
| Kernel, agent folder format and protocol | `kernel/` (incl. `kernel/PROTOCOL.md`) | MIT, open source |
| Bot client and wire protocol crates, guest scripts | `serve/host/crates/guest`, `serve/host/crates/wire`, `scripts/install-guest.sh`, `scripts/package-guest.sh`, `scripts/smoke-guest.sh` | MIT, open source |
| Docs and specs, top-level scripts | `docs/`, `serve/docs/`, `install.sh`, other `scripts/` | MIT, open source |
| Host, cloud, relay | `serve/host` (except the crates above and kith-mem), `serve/cloud`, `serve/relay` | FSL-1.1-MIT, Fair Source |
| Apps (Mac, iOS, web/desktop) | `apps/` | FSL-1.1-MIT, Fair Source |

FSL-1.1-MIT is Fair Source, not open source: you can read, run, modify and self-host it for any use except building a competing product or service, and each version becomes plain MIT two years after its release. The nearest `LICENSE` file to a file is the one that applies to it. The relay's Codync-derived portions keep their Apache-2.0 attribution in `NOTICE`. The "Kith" name and logo are covered by [TRADEMARKS.md](TRADEMARKS.md), not by these licenses.

Code committed before this change (2026-10-25) was published under MIT/Apache-2.0, and copies taken before then keep those licenses.

## Credits
Kith's relay is borrowed from [Codync](https://github.com/leepokai/Codync) (Apache-2.0).
