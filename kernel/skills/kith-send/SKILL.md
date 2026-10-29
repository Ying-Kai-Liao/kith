---
name: kith-send
description: Put a result, a plan card, a summary or a file on the user's phone, post a helper's status line into a chat, or push to the owner's Grok bot (`kith send-to grok`).
---
# Sending to the user's phone

`<kernel>` below is the kernel folder this skill sits in (two levels up): the same path your protocol writes before `bin/`.

- Text: `<kernel>/bin/kith-send "text"`. Files: `<kernel>/bin/kith-send --caption "text" FILE...`.
- Inside a Discord task it posts into that task's thread; otherwise it goes to the owner's own Telegram DM.
- In a Kith app chat, share files with the `send_file` tool instead (kith-send works there too, with a hint).
- It goes to the owner only, never to anyone else.
- A helper called from a chat (`KITH_VIA` is discord, telegram or slack) posts one short status line there and
  nothing else: `<kernel>/bin/kith-send --as <agent> "🎨 generating the infographic…"`.

## Grok

An agent with `grok` in its `needs:` pushes to the owner's Grok bot with `<kernel>/bin/kith send-to grok "text"`
(`--body FILE|-` for a raw JSON body, `--profile` to pick a bot).
- "sent" means Grok answered 200 with `"success":true` and started a run (its `runUuid` is printed), not that it replied.
- A 400 is a paused routine, a 401 a refused key; neither is retried.
- Grok allows 300 runs/hour overall and 200 per routine, and over a cap the run fails silently, so a 200 can still end
  in no run.
- The answer comes back on the bot's own link.
