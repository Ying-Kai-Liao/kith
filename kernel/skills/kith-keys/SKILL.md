---
name: kith-keys
description: A run needs a service's key or secret it doesn't have. How to ask the owner for it and use it without seeing it.
---
# Keys (the owner types them; you never see them)

`<kernel>` below is the kernel folder this skill sits in (two levels up): the same path your protocol writes before `bin/`.

1. A key you need and don't have is a request, not a question in chat. `<kernel>/bin/kith-auth` knows which services
   declare one (each service lists its `secrets`).
2. Call the `request_secret` tool once (`service`, `purpose`). In an owner `-p` run without the tool it is
   `requestSecret` on the run socket. The tool is only offered when the run has a host to ask. Then end your turn.
3. The owner gets a card in their chat with you: an Access card (pick an existing account's key) or a Key card (a
   private field). The host saves it with `kith-auth set <service>/<account> --stdin-json --grant <you>`. You are
   resumed with the names only (`[Kith] Kai saved GITHUB_TOKEN (github/work) for you. ...`). A no ends it: don't ask again.
4. Use it through `<kernel>/bin/kith-auth exec` (or `get`). Never print it, never write it down, never ask for it in chat.

Keys are profiles `<service>/<account>` (`^[a-z0-9][a-z0-9._-]{0,39}$`), granted per agent.

The host and app side of this (the `kith-auth` commands the host calls, the card shapes, `provideSecret`, errors) is
`docs/reference/keys.md` in the kith repo.
