# Releasing

A `v*` tag runs `.github/workflows/release.yml`: the `kith` CLI tarballs (macOS, Linux guest), the desktop app for Mac and
Linux, `SHA256SUMS` and `kith.rb`, all attached to that tag's GitHub Release. The release is published only when every
job except the Linux guest build passed; rerun a failed job and publish follows. `workflow_dispatch` runs everything
but publishes nothing (a dry run; give it a `version` or it uses `apps/desktop/package.json`'s).

## The desktop app

The app's version comes from the tag (`v1.2.3` is 1.2.3); electron-updater compares it, so it must match. Nothing is
committed for it.

Asset names have no version, so `https://github.com/Ying-Kai-Liao/kith/releases/latest/download/<name>` always resolves
(the download page links these):

| File | What |
|---|---|
| `kith-macos.dmg` | Mac, universal, Developer ID signed and notarized |
| `kith-macos.zip` | the same app; auto-update downloads it |
| `kith-linux-x64.AppImage`, `.deb`, `.tar.gz` | Linux, x64 |
| `kith-linux-arm64.AppImage`, `.deb`, `.tar.gz` | Linux, arm64 |
| `latest-mac.yml`, `latest-linux*.yml`, `*.blockmap` | the update feed (electron-updater) |

The public links on kithbot.app (https://kithbot.app/download) redirect to those files:

| Link | File |
|---|---|
| `/download/mac` | `kith-macos.dmg` |
| `/download/mac/zip` | `kith-macos.zip` |
| `/download/linux`, `/download/linux/appimage` | `kith-linux-x64.AppImage` |
| `/download/linux/deb` | `kith-linux-x64.deb` |
| `/download/linux/tar.gz` | `kith-linux-x64.tar.gz` |

Auto-update reads the feed from GitHub directly.

### Releasing the desktop app: one-time secrets

The Mac app is signed with a Developer ID Application certificate (team `7FUM8A8H72`) and notarized with an App Store
Connect API key (create it under Users and Access, Integrations; the **Developer** role is enough for notarytool).
On a tag the job fails if any of these is missing; a manual run builds unsigned instead.

```sh
base64 -i DeveloperID.p12 | gh secret set MAC_CERT_P12_BASE64 -R Ying-Kai-Liao/kith
gh secret set MAC_CERT_PASSWORD -R Ying-Kai-Liao/kith            # prompts
gh secret set APPLE_API_KEY_P8 -R Ying-Kai-Liao/kith < AuthKey_XXXX.p8
gh secret set APPLE_API_KEY_ID -R Ying-Kai-Liao/kith --body XXXX
gh secret set APPLE_API_ISSUER -R Ying-Kai-Liao/kith --body <issuer uuid>
```

The workflow maps them to electron-builder's `CSC_LINK`, `CSC_KEY_PASSWORD`, `APPLE_API_KEY` (a .p8 written to the runner's
temp dir), `APPLE_API_KEY_ID` and `APPLE_API_ISSUER`. The public Clerk key and cloud URL are plain env in the job.
The computer-use driver inside `KithScreen.app` keeps its maker's signature (not re-signed); if notarization rejects it
(hardened runtime or timestamp), the log names it.
