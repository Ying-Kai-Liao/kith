# Backups in the apps (`backups`, `backupsSet`, `accountSet`)

The host's view of the backup chain (the order kith moves a bot's turns to when its own account runs out of usage; the
failover itself is `agent/backups.rs`) and of each agent's main account, the one its turns start on (chain entry 0).
Code: `host/src/api/backups.rs`. A paired phone may call all three (Control scope).

## `backups {}`

```json
{
  "global": ["codex", "claude:work"],
  "agents": { "house": ["claude:work"] },
  "entries": [
    { "id": "codex", "harness": "codex", "account": null, "kind": "subscription",
      "ready": false, "why": "not logged in", "fix": "codex login",
      "outUntil": null, "reason": null }
  ],
  "main": {
    "global": "claude:work",
    "agents": {
      "house": { "id": "claude:work", "source": "global", "harness": "claude", "ready": true, "why": null, "fix": null }
    }
  }
}
```

- `global`: entry ids in order. `agents`: only agents with an order of their own (an agent not listed follows `global`;
  `[]` is an own, empty order).
- `entries`: every entry in an order, plus extra accounts on this Mac that are in none (what the app offers to add).
  `ready` / `why` / `fix` come from kith (`kith backup list --json`, `kith account list --json`); `fix` is the one
  command to run in the terminal. `outUntil` is epoch milliseconds while the entry is out of usage (from `backups.json`);
  `reason` is `rate_limit` or `usage_95`.
- `main.global`: the computer's main account (`kith account use ID`), null for each harness's own login. `main.agents`:
  every agent's main account as kith resolves it (`kith backup list --agent A --json`): `source` is `agent` (its own,
  `kith account use ID --agent A`), `global` or `default` (the harness's login; also when the computer's is of another
  harness); `harness` is the agent's. An entry of the chain equal to the main account is skipped by kith. A host or
  kith from before main accounts leaves `main` out or empty.
- No token, environment value or account folder is ever returned.
- kith missing or failing is an error (400) with kith's text.

## `backupsSet { agent: string|null, chain: [ids]|null }`

Replaces a whole order in one `kith account backup set` write: `agent` null is the global order. `chain: null` with an
agent drops its own order (`kith account backup reset --agent A`). Writes are serialized, the last to arrive wins, and
the answer is the same as `backups`. Ids are checked (`claude`, `claude:work`, `claude-api:main`, `codex`…) before kith
is run, and kith checks them again.

## `accountSet { agent: string|null, account: string|null }`

Chooses a main account with `kith account use ID [--agent A]`; `account: null` goes back to the default
(`kith account use --reset [--agent A]`: an agent follows the computer's again, the computer each harness's login).
Serialized with `backupsSet`; kith refuses an id outside the agent's harness family (a Codex id for a Claude agent),
which is a 400 with kith's text. The answer is the same as `backups`.

It applies from the agent's next turn: the host reads every known agent's chain again at once, so a warm session sees
that its main account changed and restarts. A session is loaded only under the account that made it (the host tags
it, `session-account:<sid>`); one made under another account isn't loaded: the agent starts a fresh session with the
latest messages and a notice says the main account changed. A change made only in the terminal applies when the host
next reads the chain (within 15 minutes).
