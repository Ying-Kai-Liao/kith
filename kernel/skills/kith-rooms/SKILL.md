---
name: kith-rooms
description: A run in a group room (room="group" on the session tag), room notes, or a question of who may see which memory.
---
# Group rooms

`<kernel>` below is the kernel folder this skill sits in (two levels up): the same path your protocol writes before `bin/`.

- `room="group"` on the `<session>` tag: more than one person reads this room, or it can't be vouched for. Only what
  every person there may see was loaded. Don't read the person file (`user.md` or `people/<id>.md`), your
  `projects/ID.md` or your session file: they may hold what someone there may not see. Use
  `<kernel>/bin/kith recall <query> [--project p] [--room r]` instead, which applies the room's rules.
- A line saying some of what you know is kept out of this room: you may tell the person asking "ask me in our 1:1";
  never say what it is.
- Who is present comes from the host: `KITH_ROOM_MEMBERS` (comma-separated person ids). Without it a group room gets
  the shared notes only. `KITH_ROOM_OWNER_ONLY=1` is a Kith app group the host just read and found to hold only the
  owner's own bots: only the host sets it, only for an owner run, never in a channel group, and it is ignored beside a
  member list or in anyone else's run. A failed read sends neither, which stays "unknown".
- Room notes (`memory/rooms/<room>/`) load and are recalled only where everyone present was in the room when the note
  was written. A hook (`bin/kith-room-ledger`, on Write, Edit and MultiEdit) records who was present in the state
  folder, not in the note; a rewrite keeps the intersection, and a note with no record counts as the owner's alone.
  Write in a room note only what everyone present may read. A Tidy promotion the owner approved changes its audience.
- A non-owner run's recall goes to the host with its run token, which says who you work for; inside that run's
  sandbox an unset `KITH_PERSON` is no one, never the owner.

Design: `docs/memory-context.md` in the kith repo.
