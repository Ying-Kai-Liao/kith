# Privacy

Kith runs your agents on your own Mac. This page lists what stays there, what leaves it, and who receives it. Each
item names the code that does it, so you can check.

## What stays on your Mac

- **Your chats.** The host stores them in a SQLite database (`~/.kith/serve-main/serve.db`; Kith Dev uses
  `~/.kith/serve`). The database isn't encrypted (`serve/host/src/store/schema.rs`, `serve/host/src/environment.rs`).
  If you invited people, their chats with your agents are stored there too.
- **Your agents' memory and files**, under `~/.kith` and each agent's folder ([docs/architecture.md](docs/architecture.md)).
- **What runs cost.** The budget ledger, `~/.kith/cost.db`, records each request's agent, model, token counts and
  price. It doesn't record what was said (`kernel/bin/kith-cost`).
- **Secrets.** Account and API keys are in the macOS Keychain (`kernel/bin/kith-auth`). Connector credentials are
  encrypted in the host's database, with the key in the Keychain (`serve/host/src/market/vault.rs`).

**Backups, if you turn them on.** `kith backup` asks first, then copies your state (`user.md`, projects, agent list),
your agents' memory and your own agents' instructions to **private GitHub repositories in your own account**.
`kith backup --nightly` repeats it every night at 03:00. Databases go in as SQL text. The copies aren't encrypted, so anyone you give access to those repositories can read them
(`kernel/bin/kith-state`, `kernel/lib/dot-kith.gitignore`, `kernel/lib/agents.gitignore`). The secrets folder, logs
and the host's chat database stay out of git.

## Usage analytics and crash reports

Kith sends usage analytics and error reports to [PostHog](https://posthog.com) (`us.i.posthog.com`). **It's on unless
you turn it off.** Each computer and each iPhone has its own switch, in Settings > Privacy. Turning it off stops
sending at once and drops anything not sent yet (`serve/host/src/analytics.rs`,
`apps/ios/Kit/Sources/KithKit/Analytics/Analytics.swift`). A phone can't change a computer's setting
(`serve/host/src/api/devices.rs`). Debug builds of the host (`cargo run`, tests) never send.

**What's sent:**

- **Which features you use.** Opening the app, creating or changing a bot, sending a message, a turn finishing,
  running a routine, a voice call, installing a connector or skill. The full list is the `Event` enum in
  `serve/host/src/analytics.rs`.
- **With each event:**
  - the bot's agent and model;
  - how many attachments a message had;
  - how long a call or turn took;
  - how a turn ended, and the kind of error if it failed;
  - the connector's catalog name;
  - the app version, the platform and the chip.
- **An ID for this computer:** a one-way hash of the host's random ID (`serve/host/src/analytics.rs`, `common`).
- **Who it belongs to:** your Kith account ID when you're signed in, otherwise a random ID made on this device.
- **Crashes and errors:**
  - the error's type;
  - where in Kith's own code it happened, with file paths cut down to Kith's file names;
  - whether it was the host, the terminal UI or the Mac app.

  An error's message isn't sent. The one exception is a fixed text written into Kith's code
  (`serve/host/src/analytics/errors.rs`). The Mac app reports a crash of its main process on its next launch. The
  crash dump itself stays on the Mac and is deleted after the check (`apps/desktop/src/main/error-reports.ts`). The
  iPhone app sends Apple's MetricKit crash reports, which name the signal and binary offsets only
  (`apps/ios/Kit/Sources/KithKit/Analytics/CrashReports.swift`).
- **Approximate location** (country and city). PostHog works it out from the IP address the events come from.

**What isn't sent:** your messages, prompts, code, files, file names or paths, bot names, credentials, or anything
your agents produce. The `kith` command sends no analytics.

The iPhone app's privacy manifest declares the same: product interaction, crash data and coarse location, linked to you,
for analytics, with no tracking (`apps/ios/Resources/PrivacyInfo.xcprivacy`).

## The Kith cloud

You need the cloud only to reach your Mac from elsewhere or to invite people. It runs on Cloudflare
(Workers, D1, Durable Objects) and signs you in with [Clerk](https://clerk.com), using Google, GitHub or Apple
(`serve/cloud`).

**What it keeps:**

- **Your account:** your Clerk user ID, your email address and when you joined (`serve/cloud/migrations/0001_init.sql`).
- **Your computers and devices:** their public keys, names, platform, app version, whether they're online, and when
  they were last seen.
- **Who can reach what:** access grants, invites and memberships. Your Mac sees the email addresses of the people
  you invite and of anyone asking for access (`serve/cloud/src/routes/host.ts`).
- **An audit log** of who did what (for example, a device added or access revoked). It's deleted after 30 days.
  Finished access requests are deleted after 30 days. Unaccepted bot invites are deleted 7 days after they expire
  (`serve/cloud/src/api.ts`, the scheduled sweep).

**What passes through:**

- **Your chats pass through encrypted end to end.** The cloud can't read them and doesn't store them. When a device
  is offline, the encrypted messages wait for it for up to 24 hours (`serve/cloud/src/relay.ts`,
  `serve/host/crates/wire/src/crypto.rs`).
- **Webhooks** that you point at your bots wait in the cloud while your Mac is offline. They're **not** encrypted
  there: the body and forwarded headers are kept for up to 72 hours, or until your Mac takes them
  (`serve/cloud/src/relay.ts`).
- **Remote screen sessions** use Cloudflare's TURN service to connect (`serve/cloud/src/routes/host.ts`).

**Deleting your account.** There's no button for it in the apps yet. When an account is deleted in Clerk, the cloud
does the following (`serve/cloud/src/routes/webhooks.ts`):

- revokes its devices, grants, memberships and invites;
- releases its computers;
- marks the account deleted.

**The account row, with the email address, is kept.** To have it removed, open an issue or ask the person who runs
your Kith.

## Push notifications

Notifications to the iPhone go through Kith's push relay (`serve/relay`) to Apple. The relay and Apple see only a
generic line. Who wrote and what they wrote are encrypted for your phone, and the phone decrypts them itself
(`serve/host/src/remote/push.rs`, `apps/ios/NotificationService/NotificationService.swift`). The relay stores
nothing.

## Services you choose

These see your data only when you set them up:

- **Anthropic, through Claude Code.** Your agents run on Claude Code, so your prompts and the files they read go to
  Anthropic under your own Claude account.
- **OpenAI Realtime and Google Gemini Live, for voice calls.** With your own key, the audio goes from your device
  straight to the provider (`serve/host/src/voice.rs`).
- **Typesafe (the Jev router).** It's on only when you add its key. It's then sent the end of your message and recent
  messages, to choose an agent (`serve/host/src/chat/router.rs`).
- **Connectors and skills** you install, and the services they connect to: Composio, MCP servers, GitHub
  (`serve/host/src/market`). Also Telegram, if you connect it (`serve/host/src/channels/telegram`).
- **Hugging Face**, from which the host downloads the local voice model (`serve/host/src/voice_local.rs`).

## People you invite

Someone you invite chats with the agents you give them, on your Mac ([docs/invited.md](docs/invited.md)).

- **You can't read their chats in the apps.** You only see the permission requests their agents send you. Their
  chats are stored on your Mac like yours (see above).
- **Bots they bring from their own computer are different:** you can see that traffic
  ([docs/kith-bot-guests.md](docs/kith-bot-guests.md)).
- The cloud keeps their account as it keeps yours.

## Questions

Open an issue on [GitHub](https://github.com/Ying-Kai-Liao/kith/issues). For anything sensitive, use the private
report in [SECURITY.md](SECURITY.md).
