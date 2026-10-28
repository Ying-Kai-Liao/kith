# kith architecture

What kith is made of, what is built, and where it is going. Read this before changing the kernel or designing a new
agent. How a run works step by step: `how-hq-runs.html`. Commands and file formats: the README.

## Concepts (agreed and built 2026-10-14)

kith has two levels, and each has the same two halves: what it **is** (shareable, versioned, the same for anyone who
installs it) and what it **has lived** (private, accumulated, yours).

| | **Is** | **Has lived** |
|---|---|---|
| **kith**, the system | the kernel: tools, PROTOCOL, service definitions | kith's knowledge: who it works for (`user.md`), projects (registry, shared maps and decisions), the team (`agents.json`), its rules (`private-words`, jobs, grants) |
| **an agent** | its role: instructions, skills, tools (the package repo) | its memory (what it knows), data (what it tracks), files (what it made) |

- "Agent" means the role plus its life, on one machine; "role" (today: package) is the part anyone can install.
- An agent's three kinds of life differ by lifecycle: **memory** is small text, versioned; **data** is databases,
  versioned as text dumps; **files** are things it made (renders, exports, images), large, not in git.
- kith's lived knowledge is not "about the user": it is what the whole system knows and runs on, shared by every agent.

The folders mirror the table:

```
~/kith -> kith/              the kith folder, anywhere (an external disk): this repo
  kernel/                   what kith is: the code                         public (this repo)
  agents/                   the agents' lives                            private repo (hq-agents)
    <name>/role/              what the agent is                          its own repo (kith-agent-<name>)
    <name>/memory/ data/ files/   what the agent has lived              (files/ not in git)
~/.kith/                   this Mac's kith, on the internal disk (like ~/.claude)
  user.md projects/ projects.json agents.json private-words
                            what kith has lived                            private repo (hq-state)
  secrets/ bridge/ sched/ cost.db store/ logs/ cache/ …
                            this machine only                            never in git
```

Projects are a level of their own (2026-10-16): a project can sit in a parent (a client or company holding several
repos, a line of research) and can have a **home**, any folder of the user's where its files already are. kith records
the pointer and never lays the folder out. A new project with no folder gets an empty one, inside its parent's home or
under the root the user chose in `kith init` (`homes` in `projects.json`, default `~/Documents`). To work on a whole
project, `kith project open ID` starts Claude Code in its home with every project it holds that is on this Mac. Agents are told the parent (one attribute on the session tag) but open its
memory or home only when the task reaches past their project, so context stays lean. A decision goes in the highest
project it holds for (`bin/project-id --info`, `kith project`).

## The model

| Part | What it is | Lives in | Status |
|---|---|---|---|
| **Plugin** | A capability: a skill, the tools it drives, and the software they need | inside an agent's `skills/` + `tools/` today | partly: no manifest or `requires` yet |
| **Role** (package) | What an agent is: identity (`agent.md`), settings, capabilities, its plugins, the skills it learned | its own git repo, `agents/<name>/role/` | built: versioned, snapshotted, committed |
| **Life** | What an agent has lived: memory, data, files, jobs | next to its role, `agents/<name>/` (the private hq-agents repo; files/ not in git) | built: never snapshotted |
| **State** | What kith has lived: user.md, projects, agents.json, rules | `~/.kith` (private hq-state repo; only these files are committed) | built: never snapshotted |
| **Service** | Something several agents may use: an MCP server, an API, a login | `kernel/services/<name>.json` | built |
| **Grant** | Which agent may use which service, as which account | `~/.kith/secrets/auth.json` (this Mac, never in git) | built |
| **Browser profile** | A set of logins and the browser that made them, opened by every browser tool an agent has; its own per agent unless granted a shared one | `browser/<name>` in `auth.json`, folder `~/.kith/browsers/<name>` (this Mac, never in git) | built |
| **Environment** | The installed software a package's plugins need, built per machine | not yet | planned |
| **Kernel** | Runs agents and connects them: `bin/kith`, `kith-pkg`, `kith-auth`, `kith-send`, the adapter | this repo: `kernel/` (`bin/`, `lib/`, `adapters/`, `services/`); no agents | built |

Rules that follow from it:
- **A package holds no state.** Sharing or upgrading an agent never touches anyone's memory or data.
- **A package holds no credentials.** It says what it needs (`needs:`); the user decides what it gets (grants).
- **Each capability has one owning agent** and is loaded only there; the split exists to save context tokens. Other
  agents call the owner as a helper instead of copying the capability.
- **An agent is a repo.** Installed from a GitHub link, changed by anyone, every good version committed; restore,
  share or keep private per agent. The kernel finds agents only through `kith-pkg` (`installed()`, `kith-pkg list`).
- **Agents refer to the kernel through placeholders** (`{{AGENT_DIR}}`, `{{PACKAGE}}`, `{{AGENTS}}`, `{{HOME_DIR}}`, `{{HQ}}`) and env vars
  (`KITH_AGENT_HOME`), never hard-coded paths, so kernel changes don't require editing every agent.

## Built

**State folder** (2026-10-14). Memory, data, `user.md`, projects and `projects.json` live in
`<home>/state` (`lib/kith_paths.py`), a git repo of its own. Databases are committed as text dumps (`kith-state dump`).
Personal details moved out of the packages too (2026-10-14);
`kith-pkg check --share` guards it. Third-party skills are linked from their repos (skills.json), never copied.
Published 2026-10-14 with fresh history (MIT). Since 2026-10-25 the license is split: MIT for kernel, bot client, wire crate and docs; FSL-1.1-MIT for host, cloud, relay and apps (see README, Licensing).

**Code in kernel/** (2026-10-15). The repo is the kith folder; all code and services moved into `kernel/` (bin, lib,
adapters, PROTOCOL.md, services), so the top level shows the system's parts: `kernel/`, `agents/`, `state/`. One
clone installs kith. `agents/` and `state/` stay ignored, their own repos; `git clean -x` at the top would reach them.

**One folder** (2026-10-14). Agents and state moved into the kith folder, `agents/` and `state/`, each still its own
repo and ignored by the kernel: one place to manage, the same repo boundaries. `~/.kith` is kith's stable
address, a link to the kith folder; one resolver (`lib/kith_paths.py`, `KITH_HOME`) is the only code that knows where things
are. Logs and caches moved to `~/Library/Logs/kith` and `~/Library/Caches/kith`; snapshots stay in `agents/.store` because
an APFS clone only costs nothing on the same disk.

**This Mac's kith: ~/.kith** (2026-10-15). `~/.kith` became a real folder on the internal disk, the way `~/.claude` is
Claude Code's: what kith has lived (the hq-state repo: `user.md`, `projects/`, `projects.json`, `agents.json`,
`private-words`) and everything only this machine needs (`secrets/`, settings, job state, the cost ledger, logs,
caches, snapshots), which its `.gitignore` keeps out of git by listing only the five it commits. The kith folder's
address moved to `~/kith`. `~/.config/kith`, `~/Library/Logs/kith` and `~/Library/Caches/kith` are gone; `<kith>/state` too.
Every run can read `~/.kith`, so `bin/kith` denies the file tools `~/.kith/secrets/` (tools get secrets from `kith-auth`).
Snapshots moved to `~/.kith/store`: off the agents' disk there are no clones, so unchanged files are hard links to the
previous snapshot's (as Time Machine and `rsync --link-dest` do), and `gc` keeps the 2 newest, the good, pinned and
running versions and anything under 2 days old, on every run; older versions are in each agent's history. The
resolver keeps the older layout working while `~/.kith` is still a link.

**Agents as repos** (2026-10-14). The kernel repo ships no agents. Each agent is a git repo in the agents folder
(`agents/` in the kith folder, ignored by the kernel; `<state>/agents.json` records each one's link so `install.sh` brings
them back on another Mac). `kith install <link>` clones one (a repo, or a folder of one via `/tree/<ref>/<path>`);
`kith-pkg resolve` commits every version that passes its checks, so the agent's git log is its version history and
`kith restore <agent> <commit>` brings any of them back as a new commit. Learned skills are part of the package
(`{{PACKAGE}}/skills/learned/`): they version, restore and share with the agent; a skill the user won't share stays
in the state folder. `kith share` creates `<you>/kith-agent-<name>` (private); `--public` refuses while the package or
any old commit carries personal details. The author's agents were split out of this repo's old history with it.

**Versions** (`bin/kith-pkg`). Agents change all the time: an agent learns a skill, the user asks it to change, or a
code agent edits it. None of that waits for running sessions. Every run is pinned to a read-only snapshot,
`~/.kith/store/<name>@<version>/` (APFS clones on the agents' disk, else hard links to the previous one); a changed package is checked first, and a failing version never runs (the last good
one does), and the version is committed to the agent's repo. `.editing` holds back a change that spans several
files. `kith versions | pin | rollback | unpin` for snapshots on disk; `kith history | restore` for every version ever.

**Services and auth** (`bin/kith-auth`). Service definitions live in the repo, needs in the package, grants on the
user's machine. MCP services start only when needed and granted, with secrets in the server process alone; tools
ask `kith-auth get`. Every secret handed out is logged. Least privilege between cooperating agents, not a sandbox.

**Model, effort and cost.** `model:` and `effort:` in `agent.md` set what an agent runs on, in `kith` runs and as an
`kith:<name>` subagent. `bin/kith-cost` keeps a ledger of every request; `kith` tags its sessions with agent, version,
model and effort, so `kith cost agents` / `kith cost agent <name>` show what each agent and each version costs.

**Harnesses** (`adapters/harness.py`, 2026-10-18). An agent runs on Claude Code or, with `harness:` in `agent.md`
(or `-H`), on codex, pi, agy, hermes, qwen, dsh or opencode. `bin/kith` still builds the run (prompt, snapshot, project,
directories, granted MCP servers, model, effort); only the last step differs. Callers ask for JSON, a session to resume
and a permission mode with `KITH_OUTPUT`, `KITH_RESUME`, `KITH_PERMISSION_MODE`, the same for every harness, and get Claude's
result shape back. Session ids from other harnesses are `<harness>:<id>`, so a session is only ever resumed by the
harness that made it. Settings permissions, hooks and per-request cost stay Claude-only.

**Permission mode** (`lib/kith_settings.py`, 2026-10-19). An owner's run starts in the first mode that is set: the
caller's (`--permission-mode MODE` or `--bypass` on `kith <agent>`, else `KITH_PERMISSION_MODE`, which the host's channels,
`kith schedule`, `kith ask` and `kith remote` set only from their own
`KITH_BRIDGE_`, `KITH_SCHED_`, `KITH_ASK_` and `KITH_REMOTE_PERMISSION_MODE`), this Mac's choice for the agent
(`agents.<name>.permission_mode` in `~/.kith/settings.json`; `kith install` asks for someone's agent and saves the
answer, `bypassPermissions` only on a yes), the agent's `settings.json` `permissions.defaultMode` (its author's
suggestion), then kith's default: `auto`, or `permission_mode` in this Mac's `~/.kith/settings.json` (kith's
own settings: kith-wide, not secret, never committed; `kith doctor` shows the one in force). `bin/kith` writes that default
into the run's settings, so only Claude Code runs get it; other harnesses tell only `bypassPermissions` apart from the
rest, so for them an unset mode stays as before. A run for anyone but the owner takes its membership's mode instead.

**Channels** (`serve/host/src/channels/`, `bin/kith-send`; 2026-10-19, in the host since 2026-10-22). The host
runs every platform and every bot (the Python `kith-bridge` that came first was removed 2026-10-27). Each bot is a
connection: a `discord/`, `telegram/` or `slack/` profile granted to `serve` (a grant can hold a list, one bot
each). Every connection runs and restarts on its own. `core.rs` holds what is the same everywhere; an adapter per
platform (`discord/`, `telegram/`, `slack/`) only receives, posts as the agent, threads and stops.
- **Rooms:** where people talk (a DM, `#home`, a group), keyed `<connection>:<chat>`, with an optional default agent
  (`/default <agent>`). An agent's Discord channel is a room defaulting to that agent; the owner's new DMs default to
  `personal`.
- **Who answers:** `@agent` (or `/agent`); in a thread, the agent that answered last; then the room's default; else,
  when the bot was addressed, it asks "who should take this?" with a button per agent, never guessing. A room with
  no default ignores talk not aimed at kith.
- **Threads:** one task each (a Discord or Slack thread; on Telegram a reply chain), belonging to the person who
  started it, with a session per agent (`{agent: session}`). The first agent owns it; one mentioned later answers in
  its own session there.
- **The queue**, the same on every platform: messages sent while an agent works wait and go to it together; Jev
  routes each (stop, correction → restart, other subject, more of the same); a Stop button and `/stop`.
- **Files:** in as local paths; out with `kith-send`, which leaves them in the run's outbox (`KITH_BRIDGE_OUTBOX`) for
  the host to post (Discord threads: the channel's webhook, as before).
- Telegram: groups too (privacy mode: commands, mentions of the bot and replies to it), Markdown shown as Telegram
  HTML. Slack: Socket Mode, `@agent` as plain text, `!command` (Slack keeps `/`). Discord: a mentionable role per
  agent for `@` autocomplete when the bot may manage roles.

**People** (`bin/kith-auth`, 2026-10-19). Other people use the owner's kith through chat. The tables live in
`auth.json`, next to grants, so agents can't change them, one fact per table:
- `people`: who someone is (name, lang, tz) and their chat accounts (`handles`, numeric ids). One is the owner;
  `kith init` names them from git.
- `members`: agent × person, with a preset and overrides (mode, deny, model, env, helpers). Access is the row
  existing. `*` is a row for every known person; a person's own row replaces it whole.
- `presets`: named permissions. Built in: `guest` (mode `default`: only what the agent's settings allow) and
  `member` (`acceptEdits`).
- Grants keyed `<agent>@<person>`, tried before the agent's own: a person's own account for that agent.
- A profile's owner (`"person"`): logins (`login` and `browser` services, or `"personal": true`) are the owner's by
  default, keys and bots shared; `"*"` shares one on purpose. A run for anyone else never gets someone else's
  profile, whatever the grants say.

A run's person is `KITH_PERSON`: set only by entry points (the host's channels from the sender's account, a schedule from the
person whose run added the job, checked against kith-auth when it is due), unset means the owner, inherited by every
helper. `kith ask agent@machine` is refused for anyone but the owner: the other machine has no id for them yet. `bin/kith` resolves it before anything else and fails closed. For anyone but
the owner, the membership's mode replaces the caller's, and the run:
- stays on Claude Code;
- sees only the kernel, the agent's own folder, its pinned package and the current project;
- is denied every other agent's folder and `user.md`;
- gets `person=`, `name=`, `lang=`, `tz=` on its `<session>` tag (PROTOCOL: "Who you work for").

The plugin refuses `kith:<name>` spawns the person may not use (`--via` the calling agent: a helper is allowed when
the caller's membership lists it). The host's channels map a sender to a person (`whois`), link accounts with `/link CODE`
(from `kith auth invite`), check access before starting, and give a thread to the person who started it.
Others are turned away from it, because its session holds what the agent saw for them. The old Discord settings
(`KITH_DISCORD_REPLY_TO`, `KITH_DISCORD_ALLOWED_IDS`, `KITH_DISCORD_OWNER_ID`) are ignored: the owner is the `people` owner
(until their Discord account is known, the owner of the one server named by `channels.discord_guild` stands in in the
host; with no server named, no one does).
- Only the owner gets a free shell: `mode=bypassPermissions` on a membership or preset needs `--trusted`; one set
  without it (by hand) runs in `default`.
- `kith auth person <id> id=<new>` renames, carrying memberships, grants, owned profiles and invites; the old id stays
  in `was`, so chat threads and the cost ledger still mean them.
- Cost per person: the ledger's runs carry `person` (`kith cost people`), and `budget=USD` on a membership caps a
  calendar month of their runs of that agent (helpers reached through it included); `bin/kith` refuses past it.
- Memory per person ([memory-per-person.md](memory-per-person.md)): the owner's memory stays in the agent's
  `memory/`; anyone else's run uses `memory/people/<id>/`, `files/people/<id>/` and `<state>/people/<id>.md`, and
  everyone shares `memory/shared/` (the craft, never a person). Their runs are denied the owner's memory and
  outputs and every other person's, by path; `kith-memory` searches only their part. Rename moves the folders;
  removal keeps them.
- `kith doctor` checks the tables: rows and grants naming no one, missing agents, logins shared with everyone, free
  shells, the old Discord settings, and Reason Network wired into Claude Code (its daemon lists every Claude Code
  session, agent runs included, as one its team peers can prompt, past kith-auth). `kith auth people [ID]
[--matrix]` shows everything: each person's accounts, owned profiles, the agents they reach and how, and the
account each service would really use in their runs.

Everything runs as one macOS user. A run for anyone but the owner has Claude Code's OS sandbox around its shell
(`kernel/bin/kith`): it reads only what the run was given, its recall and memory search go to the host with its run
token (`serve/host/src/runauth.rs`), and a shell that unsets `KITH_PERSON` (or names the owner in it) is no one, not
the owner: `kith_paths.guest_sandbox()` asks the OS (that sandbox closes the home folder), not the environment.
Hooks and MCP servers run outside it, with the environment the host set. Guests get no free shell; never give one to
someone you wouldn't give the Mac.

An agent called as a `kith:<name>` subagent shares the calling session's MCP servers, so the plugin gives every helper
a PreToolUse hook (`kith-auth helper-mcp`): a kith MCP service's tool that the helper's own role isn't granted for the
run's person is asked in the owner's session (a role limit the owner can pass) and denied in anyone else's, and a
check that fails refuses the same way. Servers that are no kith service (the plugin's own, an agent's mcp.json) are
not grant-bound and pass. A guest's run starts no shared services anyway (`bin/kith` gives it kithpermit only).

## Access and approvals

The agreed design is access-arch v3 (2026-10-25, model-checked). Code comments cite its rule numbers ("access-arch
rule 4"), its model-check fixes (M1-M8) and the approval-card invariants (A1-A9, C1-C8); this section keeps the
numbers they cite. Built state checked against main ff39195e, 2026-10-29.

A limit that isn't a secret or a login becomes a card for whoever may answer it, never a plain refusal; every yes
that lasts lives in kith-auth, where the owner can see and remove it.

**Parts, one writer each.**
- kith-auth: the ledger of lasting grants for anyone but the owner: member rows (role, access paths and domains),
  service grants `<agent>@<person>`, profile shares, aliases. `kith-auth ledger [--person ID] [--agent AGENT]` lists
  them and `ledger --rm <key>` removes one; the apps' Access lists use the host's `accessLedger` / `removeGrant`
  (`serve/host/src/api/people.rs`), which call it.
- Host card store (`serve/host/src/store/cards.rs`): requests (cards) and once grants, bound to the session that
  asked. Producers: app-chat prompts (`agent/bot/updates.rs`), `claude -p` prompts through kith-permit
  (`cards/mod.rs`), escape cards `request_access` (`cards/access.rs`), secret cards (`cards/secret.rs`).
- Launchers: the kernel for `-p` runs, the host for app chats (`agent/bot/hq.rs`). Both take grants the same way.

**Rules** (built unless marked).
- **1-2. Who answers.** The owner for the owner's runs; a member for their own run when the tool runs no code and
  every named path is inside their folders (a call naming no path fails closed; a bot's owner answers only inside
  the bot's folders); the owner for everything else. The owner's devices always show the buttons. Every producer
  asks `access::approver()` (`serve/host/src/access.rs`) and stores the answer on the card (`answerable_by`, NULL =
  the owner); the answer guard and each viewer's `canAnswer` use `may_answer = owner || (answerable_by == viewer &&
  still a member now)`. Open cards are re-derived on the sweep tick and when kith-auth's auth.json changes (a 3 s
  poll): a person who becomes a guest hands the card to the owner, one who loses the agent has it withdrawn. An
  answer counts only while the card still names that person and they still have a device here, checked in the same
  transaction that records it (M2).
- **3. Choices.** Allow once, Always allow (only where a lasting form exists: a path, a domain, a service), Deny. No
  Always on tool prompts for anyone but the owner; the owner's own chats keep Claude Code's options. An Always
  kith-auth refuses is shown as once, with why ("Allowed once only: …"). Each lasting entry records who approved it
  (`approved_by` in the ledger: "owner", a person id, or null for one from before it was recorded). The host passes a
  person's yes as `kith-auth access … --by <id>`, and kith-auth writes it only if that person is a member of the agent
  then, for themselves or their own bot, under the same lock as the write (M3). An owner's yes replaces a person's;
  a person's never replaces the owner's. When a person stops being a member of the agent (made a guest or removed
  from it), the entries they approved go, on their own row and their bots'; a rename carries them.
- **4. Delivery.** Take the session's writer claim -> reserve its once grants -> load the session with them -> take
  the grants only when it started with them; a failed load leaves them for the next activation (M4). Each loaded
  app-chat session keeps a profile digest, compared at every turn (M6); a yes, a removed grant or a demotion reloads
  that session alone before its next turn, never the agent process. A narrowing never waits: the next turn runs on
  the new profile (M7). A card's wake is taken off the owed list when a door queues it, and is `applied` only when
  the turn that carries it has started: the bot's prompt was sent, the channel's process spawned, the `-p` run got
  past its claim wait (M5). A turn that doesn't start gives the wake back; a host start gives back every wake that
  wasn't applied and offers it again (the channel inbox drops its saved copy, so it runs once). After a Stop a
  given-back wake waits for the next offer instead.
- **5. Somewhere to ask.** Host down: when `kith-permit issue` finds nothing on the host's port, it restarts the
  host's launchd job as the Mac app does (loaded from its plist when launchd doesn't know it, then kickstarted), waits
  for the port and asks for the ask token once more; still none, the run's asks are refused with "the Kith host isn't
  running". A card with no chat: planned (below).
- **6. Removal.** Built in a different shape from the design: a revoke goes through the cloud first and reaches every
  Mac before the next turn (`verification/revoke`).
- **7. An ask that parks is a job.** A `kith ask` run that ends its turn on a card exits 3 (`waiting`) with the ask
  id. The card keeps the id and shows "<agent>, asked by <caller>". The resumed turn runs as `kith ask --reply` and
  its words are that id's answer (`kith ask --wait` returns it, or the caller's session is woken, also when it ended
  with the answer unread, M8); a no or an expiry is the answer instead. Tool-prompt cards of owner runs only.
- **8. Lifecycle owns every stop.** Not built (below).

**Planned (agreed, not built yet).**
- No chat with the agent on this Mac: the card goes to the approver's inbox instead of "There is nowhere to ask".
- Removed ids are retired, so a new invite under the same name never inherits the old person's memory or logins.
- Rule 8: every stop of a session goes through the session lifecycle (lease, intent first) once F12 is built;
  today a stop is a claim release and a kill (`kith-sessions` has writer claims only).

**Must stay true** (beyond A1-A9 and C1-C8): buttons iff the answer would be accepted; no card is stranded; a yes
recorded before the deadline is in the session's next activation; every lasting grant is listed and removable;
removed means removed under every id; a card shows the scope actually granted; a `kith ask` caller hears the answer
exactly once; no turn runs on a profile wider than the current grants.

## Planned

**Bot guests**, what is left ([docs/kith-bot-guests.md](kith-bot-guests.md)): bots in rooms (Grok teams, section 7.9) and
linking a Grok account to a person (7.10). The rest is built: invites, `kith join` and `kith ask` from the client-only
Linux `kith` (`serve/host/crates/guest`, released as `kith-guest-linux-*`), the owner's approval, the host's bot
principals and limits, origin-tagged threads, and the apps' bot rows and approval card.

**People and channels**, what is left of the 2026-10-19 design (the rest is built, above; Slack waits for an app):

- **An `admin` level** (everything but the owner's logins, folder isolation off). On hold until the owner asks.
- **Sign-in links** (deferred 2026-10-19 until someone needs their own account; meanwhile people use the owner's accounts
  shared with them, `kith auth share`) for a person's own accounts: an OAuth link sent in chat (Google first), with the token stored in
  the Keychain as `google/<person>` and granted `agent@person`. Needs a Google Cloud OAuth client, and the host's channels have
  no public URL: either the loopback redirect (the person pastes back the address their browser fails to open) or a
  small relay. Undecided.
- **Shared browser logins** (deferred 2026-10-19 until two runs need one profile at once; who may use a login is
  built: `kith auth share`): the owner's browser profiles hold the agents' accounts. A profile opens in one run at a
  time, so each run gets an APFS clone, thrown away after (a login refreshed during a run is lost). `chrome-agent`
  should become a browser profile (`browser/agent`, living in the Chrome app). It can't be merged into the
  jev-browser profile yet: yt-dlp couldn't decrypt 208 of that profile's 670 cookies, which are encrypted with a
  different key. Structural too: propose first.

**Environments.** Skills increasingly need installed software (image: uv, rembg models, mermaid-cli, Chrome,
codex; video: Node kits; transcription: whisper models). Plan:
- A plugin declares `requires`, each with a `check` and an `install` command.
- `kith-pkg setup <agent>` installs what's missing into the agent's environment and does nothing if it's all there.
- A version whose requirements fail their check doesn't run, like any other failed check.
- Tools find their software through the environment (`KITH_ENV_DIR`, `PATH`), never global installs, so upgrading
  one agent's software can't break another.
- Containers are ruled out for the media agents: they need Metal, real Chrome, Keychain and the codex login.
- First case: the image agent.

**Disk.** The repo is small; the software and models agents use are not (about 32 GB on 2026-10-13: Hugging Face
models 12 GB, uv cache 12 GB, npm 4.6 GB, headless browsers 1.8 GB, design output 1.8 GB). Environments go on the
external SSD, and `bin/kith` points the tools' cache variables there (`HF_HOME`, `UV_CACHE_DIR`, `npm_config_cache`,
`U2NET_HOME`, `PUPPETEER_CACHE_DIR`, `PLAYWRIGHT_BROWSERS_PATH`). Keep uv's cache and environments on the same
volume (it links files instead of copying). `bin/kith` should fail clearly when the SSD isn't mounted. Open: whether
this applies to kith runs only or to everything on the machine.

## Later

- **Installable packages:** agents install, update and share separately now (repos). Still to come: plugin
  dependencies, and local customizations as an overlay so upstream updates still apply after you've edited one
  (today `kith update` only fast-forwards; diverged edits are merged by hand).
- **Scheduler:** launchd running agents' jobs, with tools allowed per job.
- **At scale:** one router entry instead of a description per agent; learned skills promoted only after they're
  used.
