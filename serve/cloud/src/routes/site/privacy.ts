// /privacy: PRIVACY.md at the repo root, restated for the site. It says what PRIVACY.md says and nothing else; when
// PRIVACY.md changes, change this page with it. PRIVACY.md on GitHub is the authoritative text.

import type { SitePage } from "../site";

const DOC = "https://github.com/Ying-Kai-Liao/kith/blob/main/PRIVACY.md";
const REPO = "https://github.com/Ying-Kai-Liao/kith";

export const PAGE: SitePage = {
  path: "/privacy",
  title: "Privacy · Kith",
  description: "What stays on your Mac, what leaves it, and who receives it.",
  kicker: "Trust",
  h1: "Privacy",
  lede: `Kith runs your agents on your own Mac. This page lists what stays there, what leaves it, and who receives it. The authoritative text is <a href="${DOC}">PRIVACY.md</a>, which names the code behind each item so you can check it yourself.`,
  toc: [
    ["mac", "On your Mac"],
    ["backups", "Backups"],
    ["analytics", "Analytics"],
    ["cloud", "The Kith cloud"],
    ["push", "Push notifications"],
    ["services", "Services you choose"],
    ["people", "People you invite"],
    ["delete", "Deleting your account"],
    ["questions", "Questions"],
  ],
  body: `
<h2 id="mac">What stays on your Mac</h2>
<ul>
  <li><strong>Your chats.</strong> Kith stores them in a SQLite database at <code>~/.kith/serve-main/serve.db</code>. The database is not encrypted. If you have invited people, their chats with your agents are stored there too.</li>
  <li><strong>Your agents' memory and files</strong>, under <code>~/.kith</code> and in each agent's folder.</li>
  <li><strong>What your runs cost.</strong> The ledger at <code>~/.kith/cost.db</code> records each request's agent, model, token counts and price. It does not record what was said.</li>
  <li><strong>Secrets.</strong> Account and API keys are kept in the macOS Keychain. Connector credentials are encrypted in Kith's database, with the key in the Keychain.</li>
</ul>

<h2 id="backups">Backups, if you turn them on</h2>
<p><code>kith backup run</code> asks first. It then copies your state (<code>user.md</code>, projects, the agent list), your agents' memory and your own agents' instructions to private GitHub repositories in your own account. <code>kith backup run --nightly</code> repeats this every night at 03:00.</p>
<p>Databases are saved as SQL text. The copies are not encrypted, so anyone you give access to those repositories can read them. The secrets folder, the logs and the chat database are kept out of git.</p>

<h2 id="analytics">Usage analytics and crash reports</h2>
<p>Kith sends usage analytics and error reports to PostHog (<code>us.i.posthog.com</code>). <strong>This is on unless you turn it off.</strong></p>
<h3>Turn it off</h3>
<p>Open Settings &gt; Privacy. Each computer and each iPhone has its own switch. Turning it off stops sending immediately and drops anything not yet sent. A phone can't change a computer's setting. Debug builds of the host never send anything.</p>
<p>The <code>kith</code> command sends no analytics at all.</p>
<h3>What is sent</h3>
<ul>
  <li><strong>Which features you use:</strong> opening the app, creating or changing an agent's chat in the app, sending a message, a turn finishing, running a routine, a voice call, installing a connector or skill.</li>
  <li><strong>Details with each event:</strong> the agent and model, how many attachments a message had, how long a call or turn took, how a turn ended and the kind of error if it failed, a connector's catalog name, the app version, the operating system and the chip.</li>
  <li><strong>An ID for this computer:</strong> a one-way hash of a random ID.</li>
  <li><strong>An ID for you:</strong> your Kith account ID when you are signed in, otherwise a random ID created on the device.</li>
  <li><strong>Crashes and errors:</strong> the error's type, where in Kith's own code it happened (file paths are cut down to Kith's file names), and which part of Kith it came from. Error messages are not sent, except fixed text written into Kith's own code.</li>
  <li><strong>Approximate location</strong> (country and city), which PostHog works out from the IP address the events come from.</li>
</ul>
<p>If the Mac app's main process crashes, the app reports it the next time it launches. The crash dump stays on the Mac and is deleted after the check. The iPhone app sends Apple's MetricKit crash reports, which contain only the signal and binary offsets.</p>
<h3>What is not sent</h3>
<p>Your messages, prompts, code, files, file names or paths, the names you gave your agents' chats, credentials, or anything your agents produce.</p>
<p>The iPhone app's privacy manifest declares the same: product interaction, crash data and coarse location, linked to you, used for analytics, with no tracking.</p>

<h2 id="cloud">The Kith cloud</h2>
<p>You need the cloud only to reach your Mac from somewhere else or to invite people. It runs on Cloudflare and signs you in with Clerk, using Google, GitHub or Apple.</p>
<h3>What it keeps</h3>
<ul>
  <li><strong>Your account:</strong> your Clerk user ID, your email address and when you joined.</li>
  <li><strong>Your computers and devices:</strong> their public keys, names, operating system and app version, whether they are online, and when they were last seen.</li>
  <li><strong>Who can reach what:</strong> access grants, invites and memberships. Your Mac sees the email addresses of the people you invite and of anyone who asks for access.</li>
  <li><strong>An audit log</strong> of who did what, such as a device added or access revoked.</li>
</ul>
<p>Audit entries and finished access requests are deleted after 30 days. Unaccepted bot invites are deleted 7 days after they expire.</p>
<h3>What passes through</h3>
<ul>
  <li><strong>Your chats, encrypted end to end.</strong> The cloud can't read them and does not store them. When a device is offline, its encrypted messages wait for it for up to 24 hours.</li>
  <li><strong>Webhooks</strong> you point at your agents wait in the cloud while your Mac is offline. They are <strong>not</strong> encrypted there. The body and the forwarded headers are kept for up to 72 hours, or until your Mac collects them.</li>
  <li><strong>Remote screen sessions</strong> connect through Cloudflare's TURN service.</li>
</ul>

<h2 id="push">Push notifications</h2>
<p>Notifications reach the iPhone through Kith's push relay and Apple. The relay and Apple see only a generic line. The sender and the message are encrypted for your phone, and the phone decrypts them itself. The relay stores nothing.</p>

<h2 id="services">Services you choose</h2>
<p>These services see your data only if you set them up.</p>
<ul>
  <li><strong>Anthropic, through Claude Code.</strong> An agent that runs on Claude Code sends your prompts and the files it reads to Anthropic, under your own Claude account.</li>
  <li><strong>OpenAI Realtime and Google Gemini Live, for voice calls.</strong> With your own key, the audio goes straight from your device to the provider.</li>
  <li><strong>Typesafe (the Jev router).</strong> It is used only if you add its key. It then receives the end of your message and the recent messages, to pick an agent.</li>
  <li><strong>Connectors and skills</strong> you install, and the services they connect to: Composio, MCP servers, GitHub. Also Telegram, if you connect it.</li>
  <li><strong>Hugging Face</strong>, where Kith downloads the local voice model from.</li>
</ul>

<h2 id="people">People you invite</h2>
<p>People you invite chat on your Mac, with the agents you give them.</p>
<ul>
  <li><strong>You can't read their chats in the apps.</strong> You see only the permission requests their agents send you. Their chats are stored on your Mac, like yours.</li>
  <li><strong>Bots they bring from their own computer are different.</strong> You can see that traffic.</li>
  <li>The cloud keeps their account the same way it keeps yours.</li>
</ul>
<p>How sharing works is on the <a href="/sharing">sharing page</a>.</p>

<h2 id="delete">Deleting your account</h2>
<p>The apps don't have a button for this yet. When an account is deleted in Clerk, the cloud revokes its devices, grants, memberships and invites, releases its computers, and marks the account as deleted.</p>
<p><strong>The account record, including the email address, is kept.</strong> To have it removed, open an issue or ask the person who runs your Kith.</p>

<h2 id="questions">Questions</h2>
<p>Open an issue on <a href="${REPO}/issues">GitHub</a>. For anything sensitive, use the private report described on the <a href="/security">security page</a>.</p>
<div class="note"><b>The full text.</b> <a href="${DOC}">PRIVACY.md</a> is the authoritative version. If this page and that file differ, the file is right.</div>
`,
  next: [
    ["/security", "Next", "Security"],
    ["/sharing", "Read", "Share an agent"],
  ],
};
