---
name: kith-project-files
description: A task that touches a project's code, its parents or held projects, or writes codebase.md or verify.md (the freshness check and both formats).
---
# Project files

`<kernel>` below is the kernel folder this skill sits in (two levels up): the same path your protocol writes before
`bin/`; `<state>` is the folder your protocol writes before `projects/`.

## Finding the project
- The `<session>` tag's `project=` comes from `-p` or from the current directory (`<kernel>/bin/project-id` prints the
  ID whose git remote or path matches). A name the user uses: `<kernel>/bin/project-id --list` shows former names too
  ("Widget" → `acme`).
- No project, but the work is about one you'll meet again: ask the user what to call it (suggest the product's name,
  not the folder's) and, once they answer, `<kernel>/bin/project-id --register <id>`. Never invent or register an ID
  yourself.
- Not every context is code: ongoing areas of life (expenses, calendar, places) get an ID too (`personal-places`);
  skip the codebase steps for them.

## Parents and holds
- `parents="…"` (closest first): the project sits in a client or company holding several repos;
  `<kernel>/bin/project-id --info ID` prints each one's home. Knowing the parent is enough to start. Open its memory or
  home only when the task reaches past this project (the client's requirements, scope, priorities, people, another of
  its repos): `<kernel>/bin/kith-memory search <words>` finds it across all of them, or read
  `<state>/projects/PARENT/decisions.md`. Write a decision in the highest project it holds for.
- `holds="…"`: you are working on a parent itself. `<kernel>/bin/project-id --children ID` says where each of its
  projects is on this Mac; read their `codebase.md` before going into their code.
- A project's home (`project-id --info`) is the user's own folder (briefs, specs, contracts, papers). Read it when the
  task needs it; write there only when the user asks, and never rename, move or tidy what is in it.

## Freshness check (when the task touches the codebase)
`codebase.md` starts with a `commit:` line. Compare it with `git rev-parse HEAD`.
- Same commit: trust the map; don't re-explore.
- Different: `git diff --stat <commit>..HEAD`. Read only the changed files that matter for the task, update the
  affected sections, and set `commit:` and `updated:` to now.
- No `codebase.md` (or not a git repo): explore once (README, manifests, top-level tree, entry points, how to test)
  and write it in the format below, under 150 lines: a map, not a copy of the code.

Other agents edit these files too: re-read right before editing and change only the sections you verified.

`codebase.md`:
```
commit: <full sha>   updated: YYYY-MM-DD   by: <agent>
# <project> — codebase map
## Purpose  ## Stack  ## Layout  ## Commands  ## Conventions  ## Gotchas
```

`verify.md` (how to prove a change works here; each line a fact you saw work or fail, with its date). Whenever you work
out something the next run would have to find again (a setup step, a flaky test, a trap), add it:
```
updated: YYYY-MM-DD   by: <agent>
# <project> — how to verify
## Tests  ## The real thing  ## Rigs and fixtures  ## Can't be tested here (and why)  ## Traps
```
