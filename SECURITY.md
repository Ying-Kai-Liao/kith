# Security

## Reporting a vulnerability

Report it privately through GitHub: the repository's **Security** tab, **Report a vulnerability**. Please don't open
a public issue for it. Include what you found, how to reproduce it, and the version (`kith --version`, or the app's
About window).

## Supported versions

Kith is at 0.1. Fixes go into the latest 0.1.x release only.

| Part | Version | Where it's set |
|---|---|---|
| `kith` command and host (`kith-serve`) | 0.1.0 | `serve/host/Cargo.toml` |
| Mac app | 0.1.0 | `apps/desktop/package.json` |
| iPhone app | 0.1.0 | `apps/project.yml` (`MARKETING_VERSION`) |

## How Kith is put together

The agents run on your own Mac. The host, `kith-serve`, keeps your chats there and runs each agent with Claude Code
(`kernel/bin/kith`). Phones, other computers and invited people reach the Mac through the Kith cloud, a set of
Cloudflare Workers (`serve/cloud`). The cloud passes encrypted messages along and keeps the account records.

### On the Mac

- **The local API** takes a bearer token, and only from this computer. The check is constant-time
  (`serve/host/src/api/http.rs`, `authorized`). The token is stored in the host's database and in a mode 0600 file
  in the data folder (`serve/host/src/main.rs`).
- **The host listens on all interfaces** by default (`--bind 0.0.0.0`, `serve/host/src/main.rs`), so a phone on the
  same network or on Tailscale can reach it. Anything that doesn't come from the Mac itself has to use the encrypted,
  paired channel described below.
- **Pairing** uses a one-time code. A code lasts 10 minutes, at most 5 can be open at once, attempts are rate
  limited, and codes are kept only in memory (`serve/host/src/api/devices.rs`).
- **The computer's identity key** is in `identity.json`, mode 0600, outside the database
  (`serve/host/src/remote/identity.rs`).
- **Credentials you give connectors** are encrypted (XChaCha20-Poly1305) in the database. The master key is kept in
  the macOS Keychain (`serve/host/src/market/vault.rs`).
- **Your own agents run with your permissions.** Claude Code's permission prompts and classifier are what guard
  them, not a sandbox (`kernel/bin/kith`, `kernel/bin/kith-auth`). Turning on `bypassPermissions` or `auto` for an
  agent requires `kith-auth --trusted`.
- **People you invite, and their bots, run in a sandbox.** It uses Claude Code's seatbelt sandbox and refuses to start
  when the sandbox isn't available. Commands can't run outside it, and the only network it allows is the host's
  socket. Deny rules keep your secrets folders and keys out of reach (`kernel/bin/kith`,
  `kernel/lib/kith_guard.py`). Only Claude Code may run for someone who isn't the owner. A non-owner run won't start
  without the memory-write check (`kernel/bin/kith`). The design and its threat model are in
  [docs/kith-bot-guests.md](docs/kith-bot-guests.md), section 8.

### Between your devices

- **Messages between a phone or browser and the Mac are encrypted end to end.** Keys are agreed with X25519 and
  HKDF-SHA256, frames are sealed with ChaCha20-Poly1305, and devices sign with Ed25519
  (`serve/host/crates/wire/src/crypto.rs`). Channel keys are replaced after 24 hours
  (`serve/host/src/remote/channel.rs`). The cloud relay forwards the frames without reading them, and never stores
  chat (`serve/cloud/src/relay.ts`). A phone that is offline gets its messages from a mailbox of ciphertext. Items
  in it expire after 24 hours.
- **Requests to the cloud are signed** with the device's Ed25519 key (`Kith-Sig`). Each signature carries a timestamp
  and a single-use nonce (`serve/cloud/src/auth.ts`).
- **Device keys:**
  - The iPhone app keeps them in the Keychain, readable only after the first unlock and only on this device
    (`apps/ios/Kit/Sources/KithKit/Client/DeviceIdentity.swift`).
  - The web client uses a non-extractable WebCrypto key (`apps/desktop/src/renderer/web/keystore.ts`).
  - The Mac app stores its key seed with Electron's `safeStorage` (`apps/desktop/src/main/cloud.ts`).
- **Push notifications** carry a generic line that the push relay and Apple can see. The real text is sealed for the
  device and decrypted on the phone, with no network (`serve/host/src/remote/push.rs`,
  `apps/ios/NotificationService/NotificationService.swift`). The APNs key stays in the relay (`serve/relay`).

### Releases

- Each release lists its files' SHA-256 sums in `SHA256SUMS`. From v0.1.0 that file is also signed with minisign.
  The public key is [packaging/minisign.pub](packaging/minisign.pub), key id `7FF80CDEC1769B21`. How to check a
  download by hand is in [packaging/README.md](packaging/README.md).
- `scripts/install-guest.sh` checks the signature before it installs.
- The Mac app can't be signed or notarized until Kith joins the paid Apple Developer Program ([docs/install.md](docs/install.md)).

## Known gaps

These are true of 0.1. Most are planned work, listed here so you can decide for yourself:

- **`scripts/install.sh` checks only the checksums, not the signature** (`scripts/install.sh`). To check it yourself,
  follow [packaging/README.md](packaging/README.md).
- `install-guest.sh` installs an unsigned build when `KITH_ALLOW_UNSIGNED=1` is set.
- The minisign key that signs releases is kept as a GitHub Actions secret, without a passphrase.
- **The Mac app isn't signed or notarized**, so macOS can't check where it came from.
- **Chats are stored unencrypted** in the host's SQLite database (`~/.kith/serve-main/serve.db`). That includes the
  chats of people you invited.
- **Backups are unencrypted.** `kith-state` commits your state and the agents' memory, with databases as SQL dumps, to
  private GitHub repositories that it creates (`kernel/bin/kith-state`). Anyone with access to those repositories can
  read them.
- **Webhooks wait in the cloud unencrypted.** When the Mac is offline, the cloud holds a webhook's body and its
  forwarded headers, `Authorization` included, for up to 72 hours. Hook keys are stored as given
  (`serve/cloud/src/relay.ts`, `serve/cloud/src/hooks.ts`).
- **On the Mac app, `safeStorage` can fall back to a plain file.** When the system can't encrypt, the device key seed
  and the account token are written as a mode 0600 file instead (`apps/desktop/src/main/cloud.ts`,
  `apps/desktop/src/main/account.ts`). The iPhone's device keys are in the Keychain, not the Secure Enclave.
- **Accounts can't be deleted from the apps** yet ([PRIVACY.md](PRIVACY.md)).
