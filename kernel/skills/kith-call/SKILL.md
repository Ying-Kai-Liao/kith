---
name: kith-call
description: Ring the person you work for when something needs their voice now (not to deliver a result).
---
# Calling the person you work for

`<kernel>` below is the kernel folder this skill sits in (two levels up): the same path your protocol writes before `bin/`.

- `<kernel>/bin/kith-call "short reason"`; in a Kith app chat the same thing is the `call_person(reason)` tool of the
  `kith-files` server. It shows as an incoming call in their open Kith app (no push).
- It prints and exits `ringing` 0, `declined` 3, `missed` 4, `busy` 5, `rate-limited` 6 or `not-allowed` 7.
  `--wait` holds up to 50 s for the answer; otherwise only `ringing` and the refusals come back.
- `--result "TEXT"` hands the host what the call is about (a bug and its PR link); a declined or missed call then
  leaves it in the chat under the reason, so the result is not lost with the call.
- At most one call per 10 minutes and 6 a day per agent. Never call again to chase an answer. A declined or missed
  call leaves "<you> tried to call: <reason>" in the chat.
- The call goes only to the person this run belongs to. The host gives a run a key for the person it serves (an app
  chat, a routine the host runs, a Telegram, Discord or Slack run for the owner or an invited person), and the call
  lands in that agent's main app chat with them. A run without the key (a terminal session, a launchd schedule
  outside the host) gets `not-allowed`.
- If they accept, the call opens with a turn telling you to greet them and say why you called: you are on a voice call.
