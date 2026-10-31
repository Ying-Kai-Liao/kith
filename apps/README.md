# Apps

## Usage analytics key (PostHog)

The iOS app sends usage analytics to our PostHog project (US cloud, `https://us.i.posthog.com`).
The project key is a public, write-only PostHog key and is built in as the default
(`Analytics.defaultKey`), so every build reports analytics unless the user turns off
"Share usage analytics" in Settings.

To override it for a local build, put another key in a gitignored `Secrets.plist` that the
build copies into the app (the "Kith environment" build phase in `project.yml`):

```sh
cp apps/shared/Config/Secrets.example.plist apps/shared/Config/Secrets.plist
# put the project key in the POSTHOG_API_KEY string
```

On Kai's Mac the key sits in `~/.kith/secrets/posthog-key`; `sh apps/tools/write-secrets.sh`
writes `Secrets.plist` from it (it never prints the key). An empty or missing value falls back
to the built-in default.

The desktop app sends nothing to PostHog itself; it goes through the host, which holds its own key.

## Host and accounts

The Mac app runs against `kith-serve` (built from `serve/host`): dev data in `~/.kith/serve` on
port 19224, launchd job `dev.kith.serve`; main uses `~/.kith/serve-main`, 19225 and
`dev.kith.serve.main`. If `kith serve start` already installed the job, the app adopts it and never
rewrites its plist. Inside the app, `Contents/Resources/kith-serve` is the service binary (the plist
and the binary-hash check point at it) and `Contents/Resources/kith/` is the release layout: `bin/kith`
(the same crate's `kith` name, universal) with `share/kith/kernel` beside it, filled by
`scripts/stage-kernel.sh` (the script `scripts/package-release.sh` uses too). So
`"…/Resources/kith/bin/kith" --version`, `serve status` and `doctor` work as an installed `kith` does.
Accounts are off until a build is given a Clerk application: `shared/Config/*.plist` stay empty
(never commit a key), and the sign-in buttons stay hidden or disabled, so Kith runs on QR pairing
alone. To turn them on, set two variables in the environment of the build (or of a development run):

- `KITH_CLERK_PUBLISHABLE_KEY`: the Kith Clerk app's publishable key (`pk_test_…` / `pk_live_…`), with
  Google, Apple and GitHub enabled and `kith://callback` and `kith-dev://callback` (plus the iOS
  bundle ids' `://callback`) in its native redirect allowlist. The Apple button shows only on Main builds
  with a `pk_live_…` key; the development instance can't do Apple.
- `KITH_CLOUD_URL`: the Kith cloud's `https://` address (`https://api.kithbot.app` for Main builds).

The desktop build writes them into `resources/account-config.json` (`tools/account-config.mjs`, run by
`npm run dev` and `dist:*`); the iOS build patches them into the app's `AccountConfig.plist` in the
"Kith environment" build phase. Unset or empty keeps the plist's (empty) value. The relay URL comes
from the host's `config.json` (`relay_url`) and travels
in the pairing link; the apps need no relay setting.

## Installing on your own iPhone (free Personal Team)

The committed `DEVELOPMENT_TEAM` is not yours. Pass your team on the command line (or set it
in Xcode, Signing & Capabilities) and switch the iOS app to the entitlements a free team can
sign (no Push, Sign in with Apple or communication notifications, so Apple sign-in stays hidden; app groups and the
keychain group are kept):

```sh
xcodebuild build -project apps/Kith.xcodeproj -scheme "iOS Dev" -configuration DevDebug \
  -destination 'generic/platform=iOS' -allowProvisioningUpdates \
  DEVELOPMENT_TEAM=<your team id> KITH_PERSONAL_TEAM=YES
```

In Xcode: open `apps/Kith.xcodeproj`, scheme "iOS Dev", pick your phone, set the team on the
three targets, add the user-defined build setting `KITH_PERSONAL_TEAM = YES` to the project,
Run. Then on the iPhone trust the developer: Settings, General, VPN & Device Management.
Profiles from a free team expire after 7 days.
