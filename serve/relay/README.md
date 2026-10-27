# Kith relay

Cloudflare Worker that holds the APNs key and forwards pushes for the Kith host.
The phone exchanges its APNs token for an AES-GCM **ticket** (`POST /register`);
hosts only ever see tickets (`POST /push`). A ticket is a bearer capability: anyone holding it can ask this Worker to push
to its device. Only the Worker can decrypt the raw APNs token. Do not log tickets.

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

`wrangler deploy` prints the new `https://kith-relay.<account>.workers.dev` URL. Until the apps are switched to it, the iOS app keeps pointing at the previous Worker; once `kith-relay` has a public URL and the APNs secrets, set `KITH_PUSH_RELAY_HOST` in `apps/project.yml` and `apps/Kith.xcodeproj` to its host and ship the app (phones re-register with it on launch).

Secrets are set per Worker, so `kith-relay` starts with none of the old Worker's: set all of the above (the APNs key and `TICKET_KEY` can be new, but a new `TICKET_KEY` means every phone re-registers on launch).
Rotating `TICKET_KEY` invalidates every ticket; phones re-register on launch.

## Test

```bash
npm test        # ticket seal/open round trip, `mutableContent` → `mutable-content: 1`
npm run typecheck
```

## Notifications and Live Activities

See the [notification design](../docs/design/push-and-live-activity.md) for event
copy, encrypted content, ActivityKit payloads, configuration and device acceptance.
The Worker handles alerts and updates/ends for activities started on the phone.
Dynamic Island shares the ActivityKit update stream. This Worker does not start
activities remotely or refresh Home Screen widgets.
