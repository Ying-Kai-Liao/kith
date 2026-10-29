---
name: kith-send
description: Put a result, a plan card, a summary or a file on the user's phone, or post a helper's status line into a chat.
---
# Sending to the user's phone

`<kernel>` below is the kernel folder this skill sits in (two levels up): the same path your protocol writes before `bin/`.

- Text: `<kernel>/bin/kith-send "text"`. Files: `<kernel>/bin/kith-send --caption "text" FILE...`.
- Inside a Discord task it posts into that task's thread; otherwise it goes to the owner's own Telegram DM.
- In a Kith app chat, share files with the `send_file` tool instead (kith-send works there too, with a hint).
- It goes to the owner only, never to anyone else.
- A helper called from a chat (`KITH_VIA` is discord, telegram or slack) posts one short status line there and
  nothing else: `<kernel>/bin/kith-send --as <agent> "🎨 generating the infographic…"`.
