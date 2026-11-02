# Moving from hq

A Mac that ran hq keeps working untouched: `~/.hq` stays its state, the `dev.hq.*` jobs keep ticking and the `hq`
command still answers. `kith migrate run` moves it to the Kith names, and `kith migrate run --undo` moves it back.

```
kith migrate run --dry-run    every step, nothing changed
kith migrate run              do it
kith migrate run --undo       reverse it exactly
kith doctor               "migration" says unmigrated, migrated or partly migrated, and what is still on an hq name
```

## What it does

1. **backup** `~/.hq` is copied (`cp -a`) to `~/.kith-migrate-backup-<time>/dot-hq`, next to a copy of every plist, link,
   plugin registry file and settings file it will change. kith never deletes this folder.
2. **stop** the `dev.hq.*` jobs are booted out, so nothing runs while the state moves.
3. **move** every entry of `~/.hq` moves into `~/.kith` (same disk: a rename, so the state repo's `.git` and its remote
   come along), `~/.kith/.migrated` is written and `~/.hq` becomes a link to `~/.kith`. `~/.kith` may already hold
   `serve/` (kith serve's data): it stays. A name in both folders (other than an empty folder) stops the migration before
   anything moves.
4. **exclude** the state repo is now `~/.kith`, so what was already there (`serve/`, `hq-serve/`) goes into
   `.git/info/exclude`: `kith backup run` never commits it.
5. **home** `~/kith` becomes a link to what `~/hq` leads to; `~/hq` stays.
6. **bin** `~/.local/bin/kith` and `hq` lead to `~/kith/kernel/bin/kith`.
7. **jobs** each `dev.hq.<x>` becomes `dev.kith.<x>`. sched and backup are reinstalled by their own installers
   (`kith-sched --install`, `kith-state backup --nightly`), so the plist is the one a fresh install writes; the others
   (cost, live, house, monitor, ...) have their plist rewritten (`~/.hq` to `~/.kith`, `~/hq` to `~/kith`,
   `kernel/bin/hq-*` to `kith-*`). `dev.kith.serve` keeps running; it is restarted only when its plist names `~/.hq`.
8. **plugin** Claude Code: marketplace `kith` added, `kith@kith` installed, `hq@hq` and the `hq` marketplace removed.
9. **settings** `~/.hq` paths in `~/.claude/settings.json` and `~/.kith/settings.json` become `~/.kith` paths
   (`hq@hq` in the plugins list of the latter becomes `kith@kith`). Nothing in a git repo's history changes; the state
   repo's `settings.json` shows as modified until the next `kith backup run`.

Every step is written to `~/.kith/migrate-manifest.json` as it finishes.

## Safety

- It refuses while an agent run, a scheduled run or `kith ask` is active (`--force` goes on anyway, never past a name
  clash).
- Run it again after a crash: it continues. Run it twice after it finished: the second is a no-op.
- `--undo` puts `~/.hq` back as a real folder (the same entries, `.git` intact), removes the marker, `~/kith` (if it made
  it) and the new jobs, restores the old plists, links, plugin registry and settings from the backup, and moves the
  manifest into the backup folder. It works from a half-done migration too. A file kith wrote into `~/.kith` after the
  migration stays where it was moved to and comes back with its folder.

## Full Disk Access

The jobs run the `uv` launcher (`~/.hq/bin/hq-uv`) so macOS lets them read an external disk. macOS grants
that per file path, and the migration renames it to `~/.kith/bin/kith-uv` (every job plist and the launcher in
`~/.kith/serve/config.json` follow it; `--undo` renames it back), so `~/.kith/bin/kith-uv` has to be granted once more (System Settings,
Privacy and Security, Full Disk Access). `kith doctor` says so when a job's launcher never started its script. The grant
does not follow the old path.

## Testing

`kernel/tests/kith-migrate-check` runs all of this in a throwaway HOME with `launchctl`, `claude` and `ps` stubbed on
PATH: dry run, refusals, migrate, doctor, a second run, undo (byte-for-byte compare), a migration that fails half way
and is continued or undone.
