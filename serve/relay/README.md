# Kith relay

Cloudflare Worker that holds the APNs key and forwards pushes for the Kith host.
The phone exchanges its APNs token for an AES-GCM **ticket** (`POST /register`);
hosts only ever see tickets (`POST /push`). Only the Worker can decrypt the raw APNs token. Do not log tickets.

A ticket names the one computer the phone registered it for and expires (alerts 90 days, Live Activities 7; the
phone mints a fresh one each time it connects). The host signs each push with its identity key (`Kith-Sig`, as
for the cloud); the Worker derives the computer id from that key and refuses a ticket pushed by any other computer.
An expired ticket answers 410 `gone`, so the host forgets it, as it does when APNs or FCM says the token is dead.
Pushes are limited to 60 a minute per device, counted in memory per Worker isolate (so per Cloudflare location).

`LEGACY_TICKETS_UNTIL` (`wrangler.toml` `[vars]`) is the end of the rollout window: until then tickets from apps that
don't send `computerId` (no computer, no expiry) and unsigned pushes from older hosts still work. Set it about 30
days after the iOS app and host with this change are released; after it, older apps can't register for pushes.

## Deploy kith-relay

The Worker is named `kith-relay` (`wrangler.toml`). A first deploy, from `serve/relay`, on the Cloudflare account that should own it:

```bash
npm install
npx wrangler secret put APNS_TEAM_ID       # Apple Developer team ID
npx wrangler secret put APNS_KEY_ID        # an APNs Auth Key (Keys → Apple Push Notifications service),
                                           # not an App Store Connect API key — both are AuthKey_*.p8
npx wrangler secret put APNS_SIGNING_KEY   # contents of the .p8 file
openssl rand -base64 32 | npx wrangler secret put TICKET_KEY
npx wrangler secret put FCM_SERVICE_ACCOUNT   # optional: Firebase service-account JSON, for Android pushes
npm run deploy
```

`wrangler deploy` prints the new `https://kith-relay.<account>.workers.dev` URL. Until the apps are switched to it, `SharedStore.relayURL` in the iOS app keeps pointing at the previous Worker; once `kith-relay` is live, change that one URL and ship the app.

Secrets are set per Worker, so `kith-relay` starts with none of the old Worker's: set all of the above (the APNs key and `TICKET_KEY` can be new, but a new `TICKET_KEY` means every phone re-registers on launch).
Rotating `TICKET_KEY` invalidates every ticket; phones re-register on launch.

## Test

```bash
npm test        # ticket seal/open, payloads, FCM, and ticket binding (computer, expiry, signature, rate limit)
npm run typecheck
```

## Notifications and Live Activities

See the [notification design](../docs/design/push-and-live-activity.md) for event
copy, encrypted content, ActivityKit payloads, configuration and device acceptance.
The Worker handles alerts and updates/ends for activities started on the phone.
Dynamic Island shares the ActivityKit update stream. This Worker does not start
activities remotely or refresh Home Screen widgets.
