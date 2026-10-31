# Kith relay

Cloudflare Worker that holds the APNs key and forwards pushes for the Kith host.
The phone exchanges its APNs token for an AES-GCM **ticket** (`POST /register`);
hosts only ever see tickets (`POST /push`). Only the Worker can decrypt the raw APNs token. Do not log tickets.

A ticket names the one computer the phone registered it for and expires (alerts and VoIP 90 days, Live Activities 7; the
phone mints a fresh one each time it connects). The host signs each push with its identity key (`Kith-Sig`, as
for the cloud); the Worker derives the computer id from that key and refuses a ticket pushed by any other computer.
A refusal that is about the ticket carries `gone: true` and the host forgets it: expired (410), invalid, or
registered for another computer (403), as when APNs or FCM says the token is dead (410). A refusal that is the
request's fault (bad signature, unsigned after the rollout window) is a 403 without `gone`, so no ticket is lost;
neither is one on a provider 403, which is about the relay's own credentials.
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
cd serve/relay && npx wrangler deploy
```

Secrets are set per Worker: set all of the above (a new `TICKET_KEY` means every phone re-registers on launch).
Rotating `TICKET_KEY` invalidates every ticket; phones re-register on launch.

## Call pushes

A ring reaches a phone whose app is closed in one of two ways. A phone that registered a PushKit token
(`/register` with `kind: "voip"`, APNs only, topic `<bundle>.voip`) gets a VoIP push: `POST /push` with
`{ticket, voip: {data}}` goes out as `apns-push-type: voip`, priority 10, expiring in 60 s, with `data` as the whole
payload (no `aps.alert`). A voip ticket pushed without `voip`, or an alert ticket pushed with it, is a 400.
Any other phone gets a normal alert with `interruptionLevel: "time-sensitive"` and `sound: "default"`.

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
