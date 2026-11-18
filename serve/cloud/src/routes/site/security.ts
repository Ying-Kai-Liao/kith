// /security: SECURITY.md at the repo root, restated for the site. It says what SECURITY.md says and nothing else; when
// SECURITY.md changes, change this page with it. SECURITY.md on GitHub is the authoritative text.

import type { SitePage } from "../site";

const REPO = "https://github.com/Ying-Kai-Liao/kith";
const DOC = `${REPO}/blob/main/SECURITY.md`;

export const PAGE: SitePage = {
  path: "/security",
  title: "Security · Kith",
  description: "How to report a vulnerability in Kith, how Kith is put together, and the gaps it has today.",
  kicker: "Trust",
  h1: "Security",
  lede: `How to report a vulnerability, how Kith is put together, and the gaps it has today. The authoritative text is <a href="${DOC}">SECURITY.md</a>, which names the code behind each item.`,
  toc: [
    ["report", "Report a vulnerability"],
    ["model", "How Kith is put together"],
    ["mac", "On the Mac"],
    ["devices", "Between your devices"],
    ["releases", "Releases"],
    ["gaps", "Known gaps"],
  ],
  body: `
<h2 id="report">Report a vulnerability</h2>
<p>Report it privately on GitHub. Open the <a href="${REPO}/security">Security tab</a> of the repository and choose <strong>Report a vulnerability</strong>. Please don't open a public issue.</p>
<p>Include what you found, how to reproduce it, and the version. <code>kith --version</code> prints it, and so does the app's About window.</p>
<p>Fixes go into the latest release only. SECURITY.md lists the supported versions.</p>

<h2 id="model">How Kith is put together</h2>
<p>The agents run on your own Mac. The host, <code>kith-serve</code>, keeps your chats there and runs the agents. Phones, other computers and invited people reach the Mac through the Kith cloud, a set of Cloudflare Workers. The cloud forwards encrypted messages and keeps the account records.</p>
<div class="wrap"><table>
<thead><tr><th>The cloud</th><th></th></tr></thead>
<tbody>
<tr><td>Keeps</td><td>The account records.</td></tr>
<tr><td>Forwards</td><td>Encrypted frames between your devices and your Mac, without reading them.</td></tr>
<tr><td>Never stores</td><td>Your chats. A device that was offline collects its messages from a mailbox of ciphertext, where they expire after 24 hours.</td></tr>
<tr><td>Holds unencrypted</td><td>Webhooks waiting for a Mac that is offline. See the known gaps.</td></tr>
</tbody></table></div>

<h2 id="mac">On the Mac</h2>
<ul>
  <li><strong>The local API</strong> accepts requests only from this computer, and each one needs a bearer token. The token check is constant-time. The token is stored in the host's database and in a mode 0600 file in the data folder.</li>
  <li><strong>The host listens on all interfaces</strong> by default, so a phone on the same network or on Tailscale can reach it. Anything that does not come from the Mac itself must use the encrypted, paired channel described below.</li>
  <li><strong>Pairing</strong> uses a one-time code. A code lasts 10 minutes, at most 5 can be open at once, attempts are rate limited, and codes are kept only in memory.</li>
  <li><strong>The computer's identity key</strong> is in <code>identity.json</code>, mode 0600, outside the database.</li>
  <li><strong>Credentials you give connectors</strong> are encrypted with XChaCha20-Poly1305 in the database. The master key is kept in the macOS Keychain.</li>
  <li><strong>Your own agents run with your permissions.</strong> They are guarded by Claude Code's permission prompts and classifier. There is no sandbox around them. Turning on <code>bypassPermissions</code> or <code>auto</code> for an agent requires <code>kith-auth --trusted</code>.</li>
  <li><strong>People you invite, and their bots, run in a sandbox.</strong> It is Claude Code's seatbelt sandbox, and the run won't start if the sandbox is not available. Commands can't run outside it, and the only network it allows is the host's socket. Deny rules keep your secrets folders and keys out of reach. For anyone other than the owner, only Claude Code may run, and the run won't start without the memory-write check.</li>
</ul>

<h2 id="devices">Between your devices</h2>
<ul>
  <li><strong>Messages between a phone or browser and the Mac are encrypted end to end.</strong> Keys are agreed with X25519 and HKDF-SHA256, frames are sealed with ChaCha20-Poly1305, and devices sign with Ed25519. Channel keys are replaced after 24 hours.</li>
  <li><strong>Requests to the cloud are signed</strong> with the device's Ed25519 key. Each signature carries a timestamp and a single-use nonce.</li>
  <li><strong>Device keys.</strong> The iPhone app keeps them in the Keychain, readable only after the first unlock and only on that device. The web client uses a non-extractable WebCrypto key. The Mac app stores its key seed with Electron's <code>safeStorage</code>.</li>
  <li><strong>Push notifications</strong> carry a generic line that the push relay and Apple can see. The real text is sealed for the device and decrypted on the phone, without a network call. The APNs key stays in the relay.</li>
</ul>

<h2 id="releases">Releases</h2>
<ul>
  <li>Each release lists the SHA-256 sums of its files in <code>SHA256SUMS</code>. That file is also signed with minisign. The public key is <a href="${REPO}/blob/main/packaging/minisign.pub">packaging/minisign.pub</a>, key ID <code>7FF80CDEC1769B21</code>. How to check a download by hand is in <a href="${REPO}/blob/main/packaging/README.md">packaging/README.md</a>.</li>
  <li><code>scripts/install-guest.sh</code> checks the signature before it installs anything.</li>
  <li>The Mac app can't be signed or notarized until Kith joins the paid Apple Developer Program.</li>
</ul>

<h2 id="gaps">Known gaps</h2>
<p>Most of these are planned work. They are listed so you can judge the risk for yourself.</p>
<ul>
  <li><strong><code>scripts/install.sh</code> checks only the checksums, not the signature.</strong> To check the signature yourself, follow <a href="${REPO}/blob/main/packaging/README.md">packaging/README.md</a>.</li>
  <li><code>install-guest.sh</code> installs an unsigned build when <code>KITH_ALLOW_UNSIGNED=1</code> is set.</li>
  <li>The minisign key that signs releases is kept as a GitHub Actions secret, with no passphrase.</li>
  <li><strong>The Mac app is not signed or notarized</strong>, so macOS can't verify where it came from.</li>
  <li><strong>Chats are stored unencrypted</strong> in the host's SQLite database, <code>~/.kith/serve-main/serve.db</code>, including the chats of people you have invited.</li>
  <li><strong>Backups are unencrypted.</strong> <code>kith-state</code> commits your state and your agents' memory, with databases as SQL dumps, to private GitHub repositories that it creates. Anyone with access to those repositories can read them.</li>
  <li><strong>Webhooks wait in the cloud unencrypted.</strong> While the Mac is offline, the cloud holds a webhook's body and its forwarded headers, <code>Authorization</code> included, for up to 72 hours. Hook keys are stored as given, not hashed.</li>
  <li><strong>In the Mac app, <code>safeStorage</code> can fall back to a plain file.</strong> When the system can't encrypt, the device key seed and the account token are written to a mode 0600 file instead. The iPhone's device keys are in the Keychain, not the Secure Enclave.</li>
  <li><strong>Accounts can't be deleted from the apps</strong> yet. See <a href="/privacy#delete">privacy</a>.</li>
</ul>
<div class="note"><b>The full text.</b> <a href="${DOC}">SECURITY.md</a> is the authoritative version. If this page and that file differ, the file is right.</div>
`,
  next: [
    ["/privacy", "Read", "Privacy"],
    ["/sharing", "Read", "Share an agent"],
  ],
};
