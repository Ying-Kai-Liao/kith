# Installing Kith

Kith has three parts:

- the `kith` command and the host that runs your agents on a Mac;
- the Mac app, which carries its own copy of both;
- the iPhone app, which pairs with a Mac.

Someone who was invited to another person's Mac needs none of this: see [Joining someone's Kith](invited.md).

What works today:

| Part | Status |
|---|---|
| `kith` command | Released for macOS (Apple silicon and Intel). Checksums are signed with minisign. |
| Mac app | No download yet: the release workflow builds it but publishes only a signed, notarized app. Build it yourself for now. |
| iPhone app | In internal TestFlight testing only, not public yet. Build it from Xcode onto your own phone. |

## The `kith` command

You need macOS, curl and tar. Kith runs agents with [Claude Code](https://claude.com/claude-code), and `kith doctor` also
checks for git and gh.

```sh
curl -fsSL https://raw.githubusercontent.com/Ying-Kai-Liao/kith/main/scripts/install.sh | sh
kith doctor
```

The script:

1. Downloads the release for your Mac.
2. Checks it against the release's `SHA256SUMS`.
3. Installs it under `~/.kith`.
4. Runs `install.sh`, which links `kith`, adds the Claude Code plugin and runs `kith init`.

Run it again to upgrade. `KITH_VERSION`, `KITH_PREFIX` and `KITH_INSTALL_TARBALL` are described at the top of
`scripts/install.sh`.

Other ways to install it:

- **Homebrew:** `brew install Ying-Kai-Liao/kith/kith`, then run the script above once to set up your folder and the
  plugin. The release workflow keeps the tap current (`packaging/homebrew/README.md`).
- **From source:** `git clone https://github.com/Ying-Kai-Liao/kith ~/kith && ~/kith/install.sh`.
- **A Linux VM that only joins a computer** (a cloud bot): `scripts/install-guest.sh`. This client-only build checks
  the release's minisign signature before it installs ([packaging/README.md](../packaging/README.md),
  [kith-bot-guests.md](kith-bot-guests.md)).

`kith serve start` runs the host on this Mac. `kith-serve pair` prints a QR code for the iPhone app.

## The Mac app

There is no download yet. A release publishes the app (`kith-macos.dmg`, linked from
<https://kithbot.app/download/mac>) only once it is signed with a Developer ID and notarized
([releasing.md](releasing.md)). Kith is in the paid Apple Developer Program now; the Developer ID certificate and
notarization secrets are not set up yet. Until they are, build the app yourself, the same way CI does
(`.github/workflows/ci.yml`, job `desktop`). You need Node 24, Rust (the toolchain
pinned in `serve/rust-toolchain.toml`) and Xcode's command line tools:

```sh
cd apps/desktop
npm ci
mkdir -p build/native && npm run icons && swift ../packaging/dmg/background.swift build/dmg-background.tiff
CSC_IDENTITY_AUTO_DISCOVERY=false npm run dist:mac:dev -- --publish never
```

This builds **Kith Dev**, an unsigned app in `apps/desktop/dist/`. It keeps its data in `~/.kith/serve` and talks to
the development cloud. Main (`npm run dist:mac:main`, called **Kith**) is the build that will be notarized.

If you copy an unsigned build to another Mac, macOS blocks it the first time. To open it anyway:

1. Open the app once.
2. In System Settings > Privacy & Security, click Open Anyway.

Or remove the quarantine flag yourself:

```sh
xattr -dr com.apple.quarantine "/Applications/Kith Dev.app"
```

**What's inside the app.** You don't need to install the command first. The app carries:

- the host, `kith-serve`, which it runs as a launchd service;
- the `kith` command and its kernel, in `Contents/Resources/kith/`;
- local speech and voice helpers;
- Kith Screen, which lets agents use the screen when you allow it.

If `kith serve start` already set up the host, the app uses that one. Details are in [apps/README.md](../apps/README.md).

**What the app doesn't include:**

- **Claude Code.** Install it separately (`curl -fsSL https://claude.ai/install.sh | bash`) and sign in. The host finds
  `claude` on your login shell's PATH and in the usual places (Homebrew, `~/.local/bin`, nvm, bun, volta).
- **`kith` on your PATH.** The app doesn't add it. To run it from a terminal, install the command as above or call
  `"…/Contents/Resources/kith/bin/kith"`.

## The iPhone app

Build it onto your phone from Xcode. A free Apple account works, but its builds expire after 7 days and can't receive
push notifications.

1. Open `apps/Kith.xcodeproj` and pick the **iOS Dev** scheme and your phone.
2. Set your own team on the three targets. The committed team isn't yours.
3. Add the user-defined build setting `KITH_PERSONAL_TEAM = YES`. A free team can't sign Push, Sign in with Apple or
   communication notifications.
4. Run.
5. On the phone, trust the developer in Settings > General > VPN & Device Management.

The same build from the command line is in [apps/README.md](../apps/README.md#installing-on-your-own-iphone-free-personal-team).

**Pairing.** Pair the phone with a Mac in either of two ways:

- In the Mac app's menu bar item, choose **Pair iPhone…**.
- In a terminal, run `kith-serve pair` (installed as `~/.kith/serve/bin/kith-serve`).

Scan the QR code from the iPhone app within 10 minutes, or paste the `kith://pair?...` link. The phone reaches the Mac
on the same network. With a Kith account it can reach it from anywhere through the Kith cloud; Tailscale works too.

Signing in to a Kith account (Apple, Google or GitHub) shows only in builds made with the Kith Clerk key. Without it the
apps run on QR pairing alone ([apps/README.md](../apps/README.md#host-and-accounts)).
