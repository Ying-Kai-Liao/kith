# People in rooms: the owner's runbook

An invited person can join your group rooms and talk to your agents there. (What the invited person sees is in
[Joining someone's Kith](invited.md).) This page is for the owner: how to turn it off on a Mac, and what to expect while it is off.

## Turning it off on a Mac

People in rooms is on by default. The setting is the host's `rooms.people` row, read each time, so no restart is needed. To turn
it off on this Mac (new adds and invites that name a room are refused; people already in a room stay and their turns go on):

    sqlite3 ~/.kith/serve/serve.db "INSERT INTO kv(k, v) VALUES('rooms.people', 'off') ON CONFLICT(k) DO UPDATE SET v = 'off'"

To turn it back on: `sqlite3 ~/.kith/serve/serve.db "DELETE FROM kv WHERE k = 'rooms.people'"`. `off`, `false` and `0` all mean off.

## While it is off

- The desktop and web apps hide the Rooms part of the invite sheet and the People list of a room (they read the setting; see
  `PeopleView.tsx` and `RoomPeople.tsx`). People who are still in a room can't be seen or removed there until the setting is
  back on. Turn it on again to manage them. The iPhone app keeps the section while anyone still has an open stay in the room
  (`BotStore+RoomPeople.swift`), so people can still be seen and removed from it.

## Known gaps

- A Mac that has stepped its authority to the core (`kith migrate step authority`) has people in rooms off regardless of the
  row, until `held_under` lands.
- On such a stepped (switched) Mac, a person who was renamed is shut out of their rooms until the `held_under` fix lands.
