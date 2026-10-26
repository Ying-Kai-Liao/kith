# Kith cloud

Cloudflare Worker for accounts and the off-LAN relay (spec: [docs/reference/remote-relay.md](../docs/reference/remote-relay.md)).

- **`/v1` API** (`src/api.ts`, route groups in `src/routes/`): Clerk-authenticated account routes (devices, claims, computers, access
  requests, grants), `Kith-Sig`-authenticated host routes (`/v1/host/*`), the Clerk webhook, and a
  15-minute cron that expires requests and prunes nonces, and the public routine webhook route
  `POST /v1/hooks/:computerId/:routineId` (spec §7.8). D1 schema: `migrations/` (later files rebuild some tables).
- **`ComputerRelay`** (`src/relay.ts`): one Durable Object per computer (SQLite storage, WebSocket
  Hibernation). It admits device sockets against the host-signed ACL, forwards channel frames it can't
  read, reports presence, holds the offline mailbox, and queues routine webhook deliveries (`hookbox`,
  checked against the keys the host registers; `src/hooks.ts`). It is reachable only through requests the Worker
  builds itself (`X-Kith-Internal: 1`); `/internal/*` has no public route.
- **Auth** (`src/auth.ts`): Clerk session JWTs via `@clerk/backend` (`CLERK_SECRET_KEY` → JWKS fetched and
  cached per isolate; `CLERK_JWT_KEY` → networkless, dev/e2e only) and Ed25519 request signatures
  (WebCrypto).

The channel relay sees ciphertext and routing metadata. The account API also stores account/device metadata. It can shorten access (revoke, block) but never grant
it: new devices come from the host's own QR pairing or its approval, and the ACL is signed by the host.

## Test

```bash
npm ci
npm run typecheck
npm test          # vitest in workerd: vectors, Kith-Sig, API, DO relay/mailbox
```

`test/ref.ts` is a device/host reference implementation on `@noble/*`, checked against
`docs/reference/fixtures/remote-relay-vectors.json` and reused by the relay tests and the e2e harness.

The vitest pool's bundled workerd may trail wrangler's; `vitest.config.ts` pins the test compatibility
date to the newest one it supports. `wrangler.toml` keeps the deploy date.

### End to end (spec §12.1)

```bash
(cd ../host && cargo build)
npm run e2e       # local D1 + wrangler dev + real kith-serve + test/e2e/phone.ts
```

Env: `KITH_SERVE_HOST_BIN` (default `$CARGO_TARGET_DIR/debug/kith-serve`, else `../host/target/debug/kith-serve`), `KITH_E2E_CLOUD_PORT` (default
8787), `E2E_VERBOSE=1` to stream wrangler and host logs. Needs `python3` for the fake agent.

## Deploy

Deploys go through the `kith-dev` and `kith` environments (see Kith below). The top level of `wrangler.toml` is only what the tests and `wrangler dev` run on (a local D1, no routes, no Clerk instance), so always pass `--env`; `--env local` is for `wrangler dev` and the e2e harness.

| Name | Kind | Purpose |
|---|---|---|
| `CLERK_SECRET_KEY` | secret | JWKS for session tokens |
| `CLERK_WEBHOOK_SECRET` | secret | Svix signature on `/v1/webhooks/clerk` (`user.deleted`, `user.updated` for banned or locked users) |
| `CLERK_ISSUER` | var | Clerk Frontend API URL; tokens must carry this `iss` |
| `CLERK_AUTHORIZED_PARTIES` | var, optional | comma-separated; a token with `azp` must match |
| `WEB_ORIGINS` | var, optional | comma-separated origins that host the web client. Only these get CORS (`Authorization`, `Content-Type`, `Kith-Sig`) and may open a browser socket. Unset = no CORS, no web sockets |
| `WEB_SOCKET_MAX_AGE` | var, optional | seconds before the relay closes a web socket with 4100 (default 3600) |
| `CLERK_JWT_KEY` | var, dev/e2e only | public key for networkless verification |

### Kith

Kith runs its own copy on its own Cloudflare account, at workers.dev. `kith-dev` serves every local build and
takes sign-ins from the Kith Clerk development instance (issuer set in `[env.kith-dev]`; steps below). Until Kai sets
its Clerk secrets, the account routes answer 401 and only QR pairing admits devices. Release builds use the `kith`
environment (below), which has no Clerk instance yet and keeps `https://clerk.invalid`.

```bash
npx wrangler login                                   # once, in a browser
npx wrangler d1 create kith-dev                      # put its database_id in [env.kith-dev]
npx wrangler d1 migrations apply kith-dev --env kith-dev --remote
npx wrangler deploy --env kith-dev                   # → https://kith-cloud-dev.<account>.workers.dev
curl https://kith-cloud-dev.<account>.workers.dev/v1/health
```

The host's default for each build is in `host/src/environment.rs` (`cloud_url`); `KITH_SERVE_CLOUD_URL` overrides it
for one host, `KITH_SERVE_CLOUD=off` turns the cloud off. The phone learns the URL from the QR code.

### Kith accounts (the `kith` environment)

Sign-in with Google, Apple and GitHub, claiming a Mac to an account, and the account becoming the owner person's
`kith` handle in kith-auth (`kith-auth who kith <clerk user id>`). Steps marked **[Kai]** need your Clerk, Apple or
Cloudflare login; the rest are one-line commands. Nothing secret goes in the repo.

1. **[Kai]** Clerk dashboard (https://dashboard.clerk.com) > Create application > name it `Kith` > turn on Google, Apple and GitHub > Create.
   (Apple needs your Apple Developer Services ID, team id and key: Clerk's SSO connections page walks through it.
   Google and GitHub work with Clerk's development credentials until you want your own.)
2. **[Kai]** Clerk > API keys: copy the **Publishable key** (`pk_...`) and the **Frontend API URL** (the issuer, `https://<name>.clerk.accounts.dev`, or your own domain). Copy the **Secret key** (`sk_...`) too, for step 6.
3. Put the issuer in `wrangler.toml`: `[env.kith]` > `vars` > `CLERK_ISSUER` (replace `https://clerk.invalid`).
4. `npx wrangler login` (once, in a browser) **[Kai]**
5. `npx wrangler d1 create kith`, then put the `database_id` it prints into `[env.kith]` > `d1_databases` (replace the `0000...` placeholder).
6. `npx wrangler secret put CLERK_SECRET_KEY --env kith` (paste the `sk_...` key when asked) **[Kai]**
7. **[Kai]** Clerk > Webhooks > Add endpoint: `https://kith-cloud.<your-account>.workers.dev/v1/webhooks/clerk`, events `user.deleted` and `user.updated`. Copy the **Signing secret** (`whsec_...`), then `npx wrangler secret put CLERK_WEBHOOK_SECRET --env kith`. (Deploy once first, step 9, to learn the URL; set the webhook and this secret after.)
8. `npx wrangler d1 migrations apply kith --env kith --remote`
9. `npx wrangler deploy --env kith`, then `curl https://kith-cloud.<your-account>.workers.dev/v1/health`
10. App builds: set `KITH_CLERK_PUBLISHABLE_KEY=pk_...` and `KITH_CLOUD_URL=https://kith-cloud.<your-account>.workers.dev` in the environment when you build the desktop and iOS apps (the apps pick them up at build time; see `apps/README.md`).
11. On each Mac, keep a kith-auth owner person (`kith-auth person <you> owner`). When the Mac is claimed, the host binds `kith <clerk user id>` to that person; leaving the account removes it. The Settings "Link Telegram" button asks the host for `linkCode`.

The rule stays: the cloud can only shorten a device's access, never grant it; the host decides.

### Production on kithbot.app

`--env kith` is the production cloud. It serves two Cloudflare custom domains: `api.kithbot.app` (the API; check with
`curl https://api.kithbot.app/v1/health`) and the apex `kithbot.app`, which for now serves only the invite pages
(`https://kithbot.app/i/<computerId>/<inviteId>#t=<token>`, the page that opens Kith; the token stays in the fragment
and never reaches the cloud). A landing page comes later.

- Clerk webhook endpoint: `https://api.kithbot.app/v1/webhooks/clerk` (events `user.deleted`, `user.updated`).
- The Clerk production instance runs on kithbot.app (issuer `https://clerk.kithbot.app`). Add the CNAME records Clerk
  lists in Cloudflare DNS as **DNS only** (grey cloud, not proxied), or Clerk cannot verify or issue certificates.
- Main app builds use `KITH_CLOUD_URL=https://api.kithbot.app` with the `pk_live_...` publishable key from the Clerk dashboard (never committed). A Main host
  also defaults to `https://api.kithbot.app` when no URL is set.
- Deploy: `npx wrangler deploy --env kith`.

### Kith accounts on kith-dev

The dev worker takes sign-ins from the Kith Clerk development instance (application `Kith`, instance
`ins_3L8cSNxglgpzXdJY3Ob2IczJZ09`, Frontend API `https://adjusted-starfish-4735.clerk.accounts.dev`). Google and
GitHub work there. Apple does not: the instance uses Clerk's shared development credentials, and a free Personal
Team cannot sign Sign in with Apple anyway. `CLERK_AUTHORIZED_PARTIES` stays unset on kith-dev, because native
iOS and desktop session tokens may carry no `azp` or a non-web one.

Missing secrets degrade cleanly. Without `CLERK_SECRET_KEY`, token verification has no key and every account route
answers 401, as before. Without `CLERK_WEBHOOK_SECRET`, the webhook route answers an internal error and deletes or
bans nothing. QR pairing and the `Kith-Sig` host routes do not use Clerk and are unaffected.

1. **[Kai]** Clerk dashboard > Kith > Development > API keys: copy the secret key (`sk_test_...`).
2. **[Kai]** From `serve/cloud`: `npx wrangler secret put CLERK_SECRET_KEY --env kith-dev` (paste the key when asked).
3. **[Kai]** If migrations are pending: `npx wrangler d1 migrations apply kith-dev --env kith-dev --remote`.
4. **[Kai]** `npx wrangler deploy --env kith-dev` (or `npm run deploy:dev`).
5. **[Kai]** Clerk > Kith > Development > Webhooks > Add endpoint: `https://kith-cloud-dev.lykevin890919.workers.dev/v1/webhooks/clerk`, events `user.deleted` and `user.updated`. Copy the **Signing secret** (`whsec_...`), then `npx wrangler secret put CLERK_WEBHOOK_SECRET --env kith-dev`.
6. **[Kai]** `curl https://kith-cloud-dev.lykevin890919.workers.dev/v1/health`
7. **[Kai]** App builds for dev: `KITH_CLERK_PUBLISHABLE_KEY=pk_test_YWRqdXN0ZWQtc3RhcmZpc2gtNDczNS5jbGVyay5hY2NvdW50cy5kZXYk` and `KITH_CLOUD_URL=https://kith-cloud-dev.lykevin890919.workers.dev` (see `apps/README.md`). The publishable key is not secret, but it stays out of committed plists.

Local end-to-end of the account path (no Clerk account needed, a self-signed test key):

```bash
(cd ../host && cargo build --bin kith-serve)
npm run e2e:account   # local D1 + wrangler dev + real kith-serve in a temp home + a throwaway kith-auth
```

New computers are limited to 10 per IP per minute with the Workers Rate Limiting binding
`REGISTER_LIMITER` (`[[ratelimits]]`; each environment needs its own `namespace_id`).
Public routine webhook deliveries are limited to 60 per sender IP and hook per minute with
`HOOK_LIMITER` (same binding kind, its own `namespace_id` per environment).

## Web devices

A browser holds a non-extractable Ed25519 key and registers it as `platform: "web"` (`POST /v1/devices`). A web device is tied to a live Clerk login; native devices are unchanged.

- Register stores the JWT's `sid` in `devices.clerk_sid`; a newer sign-in with the same key re-binds it. A web key never becomes native or the reverse (409), and a revoked or retired key stays refused.
- Register and every relay upgrade need: a JWT that verifies and was minted for an origin in `WEB_ORIGINS` (`azp`), `sub` = the row's owner, `sid` = `clerk_sid`, the row not revoked, and Clerk answering `GET /v1/sessions/{sid}` with `status: "active"`. Any Clerk error fails closed. **`CLERK_SECRET_KEY` must be set on the worker for this** (kith-dev included): without it web devices cannot register or connect. Clerk dev instances allow 100 Backend API requests per 10 s; this is one call per web register or upgrade.
- `CLERK_AUTHORIZED_PARTIES` stays unset on kith-dev (native tokens); the `azp` check above is separate and always on for web devices.
- Browsers can't send `Kith-Sig`, so the socket offers `Sec-WebSocket-Protocol: kith.v1, kith-sig.<b64url(Kith-Sig value)>, kith-jwt.<JWT>`. The signature input is the usual one (`/v1/relay/device/<id>?v=1`). The answer is `Sec-WebSocket-Protocol: kith.v1`. A refusal is accept-then-close: 4404 unknown computer, 4426 needs update, 4401 no or expired session, 4403 forbidden. A web key that connects without a JWT is refused.
- The relay closes every web socket after `WEB_SOCKET_MAX_AGE` with 4100 (the client reconnects with a fresh JWT).
- `POST /v1/devices/retire`, signed by the device key alone (sign-out): revokes the key's rows, closes their sockets and writes `retired_keys`, so a later register is refused, even for a key never seen.

## Invites and memberships

The Mac invites a friend; the cloud only records who accepted, and can only remove access. The Mac binds the account
to a kith-auth person and decides the role. Link: `kith://invite?c=<computerId>&i=<inviteId>&t=<token>` (token = 32 random
bytes, base64url; the cloud sees only its sha256 hex). D1: `invites` and `memberships` (`migrations/0005_invites.sql`), at
most one active membership per (computer, account).

Host-signed (`Kith-Sig`, like the other `/v1/host/*` routes; each writes an `audit_events` row):

- `POST /v1/host/invites` `{inviteId, tokenHash, expiresAt}` (ms, at most 48 h ahead): stores the invite. Re-posting the same
  id and hash is a no-op; the same id with another hash is 409. The computer must be claimed.
- `POST /v1/host/invites/:inviteId/revoke`: marks an unaccepted invite revoked (409 once accepted; use the membership).
- `POST /v1/host/memberships/:membershipId/revoke`: ends the membership and revokes that account's grants on the computer.
- `POST /v1/host/people/revoke` `{userId}`: the owner revoked a person (an account, never the owner's own). Ends the account's
  memberships and grants on every computer the owner has, refuses its invites to any of them until undone, and tells each of
  them to pull. Repeating it is a no-op. `POST /v1/host/people/unrevoke` `{userId}` undoes it and tells them again.
- `GET /v1/host/state` adds `memberships: [{membershipId, userId, inviteId, email}]` (active only), `userId` on each grant (the
  account owning the device) and on each access request, and `revoked: [userId]` (the accounts this computer's owner revoked);
  the Mac sets each one's revoked state in its own kith-auth.

Account (Clerk):

- `POST /v1/invites/:inviteId/accept` `{token}`: creates the membership and returns `{computerId, membershipId}`. The same
  account repeating it gets the same membership. 403 wrong token or the computer's owner, 404 unknown invite, 409 used by
  another account (or already a member), 410 expired (`inviteExpired`) or revoked (`inviteRevoked`). Races have one winner.
- `GET /v1/computers` also lists computers where the account is a member; each row has `role: "owner" | "member"`.
- `POST /v1/computers/:id/access-requests` is open to a member as well as the owner. The host approves it; the cloud never
  does. Members cannot rename, remove or list grants of the computer.

Unclaiming a computer and the Clerk `user.deleted` webhook revoke the affected memberships and unused invites.
`npm run e2e:invite` runs the flow against `wrangler dev` with a fake host.

## Bot guests

A cloud agent (Grok Bot and the like) joins a Mac with a link and has no Clerk account: the bot is a device with its own
Ed25519 key, tied to one person on the Mac. Design and threat model: `docs/kith-bot-guests.md`. Same direction of trust as
invites: the cloud only records accepts and can only remove access; the Mac decides who the bot is and what it may do. The
invite token `t` never reaches the cloud. The bot sends `sha256hex(t)` to accept and the cloud stores
`sha256hex(sha256hex(t))` (the host keeps `sha256hex(t)` and checks `t` itself). D1: `bot_invites` and `bot_devices`
(`migrations/0006_bot_guests.sql`). Limits: 50 active bot devices and 100 open invites per computer; accepts are limited to 10
per IP per minute with `BOT_JOIN_LIMITER` (own `namespace_id` per environment). `audit_events` gets `botinvite.create`,
`botinvite.revoke`, `botinvite.accept` and `botdevice.revoke`. The cron sweep deletes unaccepted bot invites 7 days after they expire.

Host-signed:

- `POST /v1/host/bot-invites` `{inviteId, tokenHash, expiresAt}` (ms, at most 48 h ahead): same rules as `/v1/host/invites`
  (re-post is a no-op, same id with another hash is 409), but no claim is needed; 409 `tooMany` over 100 open invites.
- `POST /v1/host/bot-invites/:inviteId/revoke`: marks an unaccepted invite revoked (409 once accepted; revoke the device).
- `POST /v1/host/bot-devices/:botDeviceId/revoke`: sets `revoked_at`, blocks the key in the relay DO with grant id
  `bot:<botDeviceId>` (closing its sockets with 4003) and audits. Repeating it re-sends the block.
- `GET /v1/host/state` adds `botDevices: [{botDeviceId, inviteId, deviceKey, name, provider, createdAt}]` (active only).

Bot-signed (`Kith-Sig` by the bot's own key, replay checked by `sig_nonces`):

- `POST /v1/bot-invites/:inviteId/accept` `{token: "<sha256hex(t)>", name, provider?}` returns `{computerId, botDeviceId}`.
  The same key repeating it gets the same device. 403 wrong proof, 404 unknown invite, 409 `inviteUsed` (another key got
  there first), 409 `conflict` (this key already has a bot here) or `tooMany`, 410 `inviteExpired` or `inviteRevoked`
  (also for a device that was revoked), 429 `rateLimited`. Races have one winner. A successful accept makes the DO send
  `cloud.changed` to the host, which pulls `botDevices`.

Relay: the host lists a bot in the signed ACL as `{dk, grant: "bot:<id>", kind: "bot"}` (`kind` is optional and must be
`"bot"` when present). A bot socket counts against `MAX_BOT_SOCKETS = 8` per computer and 2 per key, apart from the 16 human
sockets, is limited to 60 messages and 1 MiB per 10 s (humans: 200 and 8 MiB), gets `mbox.err {code: "notAllowed"}` for any
mailbox message (an offline Mac fails fast), and can never use a pairing offer. A socket whose ACL kind changes is closed 4003.
`npm run e2e:bot` runs the flow against `wrangler dev` with a fake host.

## Remote screen relay

`POST /v1/host/screen-ice` issues one-hour Cloudflare Realtime TURN credentials to a
signed host belonging to an active account. The host first checks the calling
device's screen permission. The Worker requires `TURN_KEY_ID`, `TURN_KEY_API_TOKEN`
(server-only secrets) and `TURN_LIMITER` (12 issuances per account per minute).
The credentials are never cached in HTTP caches or stored in D1. See
[Remote screen setup and verification](../docs/features/remote-screen.md).
