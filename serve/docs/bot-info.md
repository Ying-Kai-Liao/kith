# Bot details: API contract

The detail page of a kith bot (role, memory, skills, tools, MCP servers, projects, schedules, cost, model and
package version) is drawn from what `kith <agent> --info-json` prints. The host passes it through; the apps draw it.
Same path as place folders (`place-folders.md`): kernel JSON, host API, apps.

All methods are `POST /api/<method>` (loopback bearer token) or the same call over a phone's E2E channel. They are
read-only and none is in `LOCAL_ONLY` (`api/devices.rs`): a paired phone with the Control scope may read and search
a kith bot's memory. These are the agent's own files, not Kith's bot memory (`memory`, `memoryCall`).

## `kithInfo { botId, refresh?: bool }`

```json
{ "info": { ...info-json... } | null, "error": "why", "fetchedAt": 1760000000000 }
```

- `info` is null, with no `error`, for a bot that is not a kith agent (no `hqAgent`) and for a group.
- `info` is null with `error` when kith is missing, fails, times out (45 s), prints something that isn't a JSON
  object, or no longer lists the agent. The call is never an error status for these.
- An unknown `botId` is HTTP 400.
- Per agent, a run is reused for 30 s (a failed run for 3 s). Older answers are returned at once and refreshed behind the call; the first
  call waits for kith. Two clients asking together cause one run. `refresh: true` runs kith now, or takes the answer of a run already in flight.
- `fetchedAt` is when kith ran, in unix milliseconds.

### info-json (version 1)

Passed through as is: read the keys you know and ignore the rest. Only `memory.dir` is the host's business.

```
{ "version": 1, "agent", "description", "harness", "model"|null, "effort"|null, "permission_mode"|null, "home",
  "role": { "path", "intro", "sections": [ { "title": "Not yours", "text" } ] },
  "package": { "version"|null, "remote"|null, "dirty", "unpushed", "pinned"|null },
  "skills": [ { "name", "description", "source": "package"|"learned"|"global" } ], "tools": [str],
  "mcp_servers": [ { "name", "kind", "command"|null, "url"|null } ], "plugins": [str],
  "projects": [ { "id", "files", "modified"|null } ],
  "schedules": [ { "id", "cron", "task", "paused", "last_run"|null, "last_status"|null, "next_run"|null,
                  "source": "kith"|"launchd" } ],
  "cost": { "currency": "USD", "today", "week", "month", "all", "requests_month",
            "by_model_month": [ { "model", "usd" } ], "since"|null } | null,
  "memory": { "dir": "/abs", "files": [ { "path" /*relative*/, "bytes", "modified" } ], "total_bytes" },
  "errors": { "<key>": "reason" } }
```

Values to know: `schedules[].source` says where a job comes from (`kith` = kith-sched, `launchd` = the agent's own LaunchAgent, whose `id` is its Label and `last_status` is always null); apps may ignore it.
Also: `schedules[].last_status` is `"running"`, `"done"` or null (never ok/failed), and `package.version`
is a fingerprint hash such as `393def5f55`, not a semver number. `errors` names the parts kith could not read (for example `cost`), so a page can say so instead of showing nothing.

## `kithMemoryFile { botId, path }`

```json
{ "path": "notes.md", "text": "...", "bytes": 19, "modified": 1760000000000, "truncated": false }
```

`path` is relative to `info.memory.dir` (the `path` of a `memory.files` entry or of a search hit). Reads stop at
512 KB: `text` is the first 512 KB, `bytes` the file's size, `truncated` true.

HTTP 400, with a message, for: an unknown bot, a bot that is not a kith agent (or whose agent is gone), an empty
path, an absolute path, any `..`, a path that resolves (through symlinks) outside the memory folder, a missing
path, and anything that is not a regular file.

## `kithMemorySearch { botId, query, limit?: int }`

```json
{ "hits": [ { "path": "notes.md", "line": 3, "snippet": "..." } ], "error": "why" }
```

Runs `kith memory search <query> --agent <name> --json -n <limit>`; `hits` are kith's, `path` relative to the memory
folder (`line` may be null). `limit` defaults to 20, at most 50. An empty query gives `{ "hits": [] }`. When kith
fails, `hits` is empty and `error` says why. Same 400 for an unknown or non-kith bot as above.

Each hit gets `"openable": bool`, added by the host: true only when `kithMemoryFile` would accept the hit's `path`.
Hits in the memory folder have relative paths. Learned-skill and shared-file hits keep the absolute path kith gives
and are `openable: false`; show them without a link to open.

## `kithSetPermission { botId, mode }`

```json
{ "info": { ...info-json... } | null, "error": "why", "fetchedAt": 1760000000000 }
```

The one write of this page, and the only hq setting an app can change for a kith bot: runs
`kith <agent> set permission <mode>` (this Mac's permission mode for the agent, kept in `~/.kith/settings.json`; an empty
`mode` forgets it) and answers with the agent's info read back from kith, so the app shows hq's value, not its own pick.
Every entry point of that agent (this app, Discord, Telegram) uses it from its next turn; a turn running now keeps its
mode. kith's refusal (a mode it doesn't know) is HTTP 400 with its text, and hq keeps what it had. A paired phone with
the Control scope may call it, as it may `updateBot`. Same 400 for an unknown or non-kith bot as above.

## `kithSetCallModel { botId, model }`

Same answer and errors as `kithSetPermission`. Runs `kith <agent> set call-model <model>`: the model that answers a
turn spoken in a voice call (`sonnet`, `haiku`, `opus`, `own` for the agent's own model, `default` to forget this
Mac's choice; kith's default is `sonnet`). `info.call_model` is the choice (`default` when none),
`info.call_model_effective` what applies (`""` is the agent's own).

Calls: an app sends `send { ..., via: "call" }` for a message spoken in a call (an older host ignores it, an older
app never sends it). The host adds a short voice note to that turn's prompt (spoken sentences, no markdown, send files
to the chat; the chat keeps only the spoken words) and, for a kith bot on its own Claude account, switches the live
session to the call model with `session/set_model` before the prompt, back to the agent's own model on the next typed
turn. Same session and process, so nothing relaunches; a failed switch leaves the turn on the model it had. Schedules,
channels, invited people and backup accounts never switch. A change reaches the next call turn at once.

For a kith bot the host never stores or passes its own model, effort, Fast mode or permission (`updateBot` leaves them
as they were, `kith <agent> --launch-json` is called without `-m`, `-e` or `--permission-mode`): hq is the source.
Old bots may still carry a stale copy in the host's config; it is ignored, not cleared.

## `kithAccessGrants { botId }` and `kithRemoveAccessGrant { botId, person, kind, target }`

```json
{ "grants": [ { "person": "sam", "name": "Sam", "kind": "read" | "write" | "domain", "target": "/Users/kai/docs" | "pypi.org", "since": 1760000000000 | null } ] }
```

The agent's standing "always" answers to escape cards (`serve/host/src/cards/access.rs`): what each person may reach
beyond the box in every run (`kith-auth access <agent> --json`, read from `auth.json`). `person` `*` is a grant on the
everyone row (written by hand); `since` is null for a grant written before kith-auth kept times. The Details page shows
them as an Access list, each with Remove.

`kithRemoveAccessGrant` takes one away (`kith-auth access <agent> <person> <kind> <target> --rm`, matched as listed) and
answers with the list read back. A run launched after it no longer gets the grant (`consumeAccess` asks kith-auth at every
launch); a run already going keeps the sandbox it started with. A grant still listed after the removal, an unknown
`kind`, or a `person`/`target` that starts with `-` is HTTP 400. Both are owner-only (not in `MEMBER_METHODS`): a member
gets 403. A paired phone needs the Control scope, as for every call.

An "always" that kith-auth refuses (the person uses the agent through the everyone row and has no membership of their
own, or a bot asks for more than its owner holds) stays a once grant. The card says so: its entry gets
`"outcome": "Allowed once only: it can't be kept for always (<kith-auth's reason>)"` and `selected` the once option, and
the apps show `outcome` in place of the option's own words.

## Memory files and tidy-ups (a kith agent's bot)

The files are the truth: `agents/<name>/memory/**` and the shared user layer (`user.md`, `people/`, `projects/` in the
state folder). Every call below takes `botId` (same 400 for an unknown or non-kith bot, or one whose agent is gone) and,
for a file, `shared` (bool: the user layer; default false: the agent's own `memory/`) and `name` (the path inside that
layer, `MEMORY.md`, `people/mum.md`; anything that isn't a Markdown memory file is a 400).

| Method | Arguments | Answer |
| --- | --- | --- |
| `kithMemoryList` | | `{ agent: [file], shared: [file] }`, `file` = `{ name, shared, path, size, modified }` |
| `kithMemoryRead` | `shared`, `name` | `{ name, shared, text, bytes, modified, truncated?, exists }`; a file not made yet is `text: ""`, `exists: false`; at most 512 KB |
| `kithMemorySave` | `shared`, `name`, `text` | `{ version, warnings }`: Kai's save, never refused (a size cap or a big drop only warns); `version` is `null` when nothing changed |
| `kithMemoryHistory` | `shared`, `name`, `limit?` (50, max 500) | `{ name, shared, versions: [version] }`, newest first |
| `kithMemoryDiff` | `shared`, `name`, `from?`, `to?` (version ids) | `{ from, to, diff }`; by default the version before the newest to the newest; `diff` has `- `/`+ ` lines, `  ` context, `…` gaps |
| `kithMemoryRestore` | `shared`, `name`, `version` | `{ version, warnings }`: the file becomes that version's content, as a new version (cause `restore`) |
| `kithMemoryTidyUps` | `limit?` (20, max 100) | `{ tidyUps: [tidyUp] }`, newest first |
| `kithMemoryUndoTidy` | `id` | `{ restored: [label], refused: [{ file, reason }], undone }` |
| `kithMemoryPromotions` | `status?` (`pending` default, `declined`, `approved`, `stale`), `limit?` | `{ promotions: [{ id, file, from, audience, room, projects, reason, asks, status, at, decidedAt }] }` |
| `kithMemoryAnswerPromotion` | `id`, `answer` (`approve` or `keep`) | `{ status: approved/declined/stale, run? }` |

Promotion: tidying may retag a note (frontmatter `audience`, `room`, `projects`; no file moves). A retag that keeps the
readers the same or fewer is applied as a tidy-up (change kind `retagged`, undoable). One that adds readers is stored
as a pending promotion and never applied by tidying; `asks` is the person who must answer (empty: the owner, shown as
"Ask <person>" otherwise). This call answers for the owner only: `approve` of a question asked of another person is
refused, `keep` (keep private) always works. An approval writes `approved_by` / `approved_change` into the note and
a row the kernel checks (`kith_facets.py`); `run` is the tidy-up that wrote it.

`version` = `{ id, at, author: "kai"|"agent"|"tidy"|"unknown", authorBot, cause: "edit"|"delete"|"restore"|"undo"|"tidy"|"baseline", deleted, bytes, lines }`;
`unknown` is a change made outside the hook (an editor, a shell). `tidyUp` = `{ id, at, undone, files: [{ name, shared, version }], changes: [{ kind, file, shared, text, was }] }`;
`kind` is `added`, `archived`, `reviewed` (`text` the new line, `was` the old), `noted` (a line appended to a note) or `indexed` (the MEMORY.md line naming a note).
`kithMemoryUndoTidy` puts every file the run wrote back to what it was before; a file that changed after the run (the
agent, Kai or a later tidy-up) is left alone and named in `refused` with the reason, and the run stays not undone until its
files are all back. HTTP 400 when it can restore nothing. Calls that change a file also emit the bot, so a page refetches.

Deprecated names, kept for app builds that predate the "Tidy" rename: `kithMemoryDreams` (same as `kithMemoryTidyUps`, and its
response also carries the list as `dreams`), `kithMemoryUndoDream` (= `kithMemoryUndoTidy`), `undoDream` (= `undoTidy`) and
the `dreams` field of the `memory` response (= `tidyUps`). Old version rows say author/cause `dream`; they are read as `tidy`.

## Thread summaries on rows, the "summary as the row" setting and citations

The thread's prose summary (`docs/memory-context.md`, "Summary") reaches the apps on the thread's row, with no extra call.

- **Row.** The root entry's `data.thread` gains `summary: { text, writtenAt }` once a summary exists (absent before, and for a
  forgotten thread). It rides in the entries the apps already get (`history`, `thread`, live `entry` events), so a list is
  one batch. `data.thread.summaryAudience` (`owner`, `person:<id>`, `room:<chat>` or `shared`) is sent to the owner only.
  An invited person gets `summary` only when the audience is `person:<self>` or `shared`; terminal threads (`owner`) and
  group threads (`room:`) are the owner's for now, since a room's members can't be resolved yet.
- **Setting.** `rowShowsSummary` (bool, default `false`; `false` for bots stored without it) is on every bot the host sends.
  Set it with `updateBot` `{ id, rowShowsSummary: true|false }` (owner only, like every bot setting). The host only stores it:
  an app shows `summary.text` as the row when the setting is on and a summary exists, else the first message.
- **Citation.** `resolveCitation` `{ citation: "kith://thread/<id>#entry=<seq>" }` (`<id>` is a thread's root entry id, or
  `main:<bot id>` for a bot's own chat) returns `{ thread: { id, botId, rootId }, seq, at, entries }` (`at` is the id of the
  entry with that `seq` when it is among `entries`; a thread's newest 500, or 40 around `seq` in a main chat). For anyone
  outside the thread's audience, and for a thread that doesn't exist, was forgotten, or a malformed link, the answer is the
  same `{ error: "not_shared" }`. An invited person may call it (it is on the member allow-list) and gets their own
  threads only, while their agent is still granted. The owner can resolve a member's thread too, as with `history`.
  `forgetThread` `{ thread }` (owner only, what `kith forget --thread` calls) forgets a thread and removes `summary` and
  `summaryAudience` from its row (an `entry` event follows); a forgotten thread's row is served without them either way.
