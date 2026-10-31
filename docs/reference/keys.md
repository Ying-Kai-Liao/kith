# Service keys: host and app contract

How an agent asks for a key is the `kith-keys` kernel skill (`kernel/skills/kith-keys/SKILL.md`). This page is the
other side: what the host and the apps do with the request. Owner only; owner devices with Control scope over E2E.

## kith-auth
- `kith-auth set <profile> --stdin-json [--grant <agent>]`: stdin `{"VAR":"value"}`, each value at most 1536 bytes,
  never argv or env; stdout `{"profile","service","vars","granted"}`.
- `account-state <agent> <service> --json` -> `{declared, secrets, granted, missing, profiles:[{profile, account, owner, agents}]}`
  (`missing` is judged against the agent's first granted profile).
- `keys --json`; `grant <agent> <service> <profile>`; `revoke <agent> <service> [--profile P]`.

Profiles are `<service>/<account>` (`^[a-z0-9][a-z0-9._-]{0,39}$`), granted per agent.

## Cards
- A `permission` entry, `data.card` `"account"|"key"`, `data.secret {service, vars, profile|null, account|null, purpose, replaces, state:"pending"|"saved"|"denied"|"expired"}`;
  account cards also `data.accounts [{profile, account, agents}]`. Never a value. Only the owner's devices are sent it.
- Account card: `respondPermission {entryId, optionId}` = `profile:<name>` | `another` (becomes a key card in place) | `deny`.
- Key card: only `deny` (options `[deny]`, detail "Update Kith to add the key here." for old apps). The key goes by
  `provideSecret {entryId, account, values:{VAR:string}}` -> `{saved:{service, vars, profile}, resumed}` (`account`
  required unless `replaces`; values 1..1536 bytes, no NUL, exactly `data.secret.vars`).

## Errors and guarantees
- 400: "That request is closed", "That account name can't be used", "Enter every field of the key", "That key is too
  long", "Couldn't save it to the Keychain" (card stays open); 403 for non-owners.
- The Keychain is written before the card is answered; two devices write once; after a deny or expiry nothing is
  written (`verification/secrets/`).

## Other calls
- `openKeyCard {service, profile, grant?}` -> `{opened, profile, service, grant, vars, replaces, chat, chatId}` (owner only; off the
  member and bot lists): the owner's own door, behind `kith keys add <service> --profile <account|service/account> [--for AGENT]`
  (`grant` defaults to `serve`, the host's own reads such as the Grok wake). Opens an ordinary key card with `replaces: true`
  and the profile fixed (the apps ask for no account name), so an existing profile is rotated (even one a person owns: the
  owner may) and a new one is created; the key is granted to `grant`. Lands in the owner's chat with `grant` when it is a
  chat agent (answering resumes it with the usual names-only note), else (`serve`) in the owner's first chat by name, with
  no resume. Resource `secret:<profile>><grant>`: a repeat joins the same card, and it never joins an agent's `secret:<service>` card.
  Refusals (400, fixed text): bad service/account/agent name, undeclared service or one with no secrets.
- `listKeys {}` -> `{keys:[...]}`; `revokeKey {agent, service, profile}` -> `{ok:true}`.
- Run socket `requestSecret {token, service, purpose}` -> `{ok, carded, message}` (owner token only). A service the agent
  doesn't declare (no `kernel/services/<service>.json` or role `services/<service>.json`) gets no card; its `message` names
  the file builder adds and says to ask again after.
