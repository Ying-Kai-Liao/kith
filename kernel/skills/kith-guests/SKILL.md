---
name: kith-guests
description: A refused action (a permission card, "Operation not permitted", a refused network), a run for someone other than the owner, or a bug in Kith itself.
---
# Refusals, guests and Kith bugs

`<kernel>` below is the kernel folder this skill sits in (two levels up): the same path your protocol writes before `bin/`.

- **A refused action in a one-shot run is a card, not a no.** A permission prompt in a `claude -p` run goes to the
  person who can approve it (`--permission-prompt-tool`, `bin/kith-permit`). When the answer says someone is being
  asked, end your turn and say what you were waiting for. Don't retry or work around it. Secrets and logins are never
  carded.
- **The sandbox of a guest run is a door with a knock, not a wall.** When a command fails with "Operation not
  permitted" (or the network is refused) for a file, folder or website you were asked to use, call the
  `request_access` tool (`path` or `domain`, and `reason`; `write: true` only if you must change it) and end your
  turn. The owner gets a card; a yes applies the next time you run, not during this turn, and you are resumed when it
  is answered. Don't retry, don't look for another way in, and don't ask for secrets, logins or startup files (they
  are refused outright).
- **A key** you don't have: `kith-keys`.
- **Working for someone other than the owner** (`person=` on the session tag): answer them by name and in their
  language. `user.md` and the owner's private matters aren't theirs to see: don't repeat them from memory. What kith
  denied them (an agent, a file, an account) was the owner's decision; say what you can't do.
- **A bug in Kith itself** (a kernel command that fails, a tool or the app misbehaving), not in the user's own work:
  report it once, with the user's word or when they ask. The owner's run:
  `<kernel>/bin/kith bug "<short title>" --details "<what happened, what you expected>"`; the report stays on this Mac
  with the session transcript and nothing goes to GitHub. A guest's run: the `report_bug` tool (`title`, `details`);
  it shows you the scrubbed draft (tell the person what it says) and the owner decides on a card whether it becomes a
  public issue. Never paste secrets or other people's details into it; don't retry when it says it is waiting.
