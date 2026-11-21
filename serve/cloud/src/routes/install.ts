// The CLI installation guide at https://kithbot.app/install, and https://kithbot.app/install.sh, which 302s to the
// install script on GitHub so `curl -fsSL https://kithbot.app/install.sh | sh` works. The page is static and has no
// script (the command is select-on-click), so its CSP allows none. It sits in the site's shared frame (routes/site.ts).

import type { Ctx } from "../api";
import { sitePage, type SitePage } from "./site";

const RAW = "https://raw.githubusercontent.com/Ying-Kai-Liao/kith/main/scripts/install.sh";
const SHORT = "curl -fsSL https://kithbot.app/install.sh | sh";

export async function installScript(_c: Ctx): Promise<Response> {
  // No caching, so a moved script takes effect right away.
  return new Response(null, { status: 302, headers: { Location: RAW, "Cache-Control": "no-store" } });
}

const DOCS = "https://github.com/Ying-Kai-Liao/kith/blob/main/docs/install.md";

export const PAGE: SitePage = {
  path: "/install",
  title: "Install Kith",
  description: "Install the kith command line tool on your Mac. The Mac app is a client of it.",
  h1: "Install Kith",
  lede: "Kith is CLI first. The <code>kith</code> command is the one thing you install; the Mac app is a client that uses it.",
  toc: [
    ["what", "What the CLI is"],
    ["requirements", "Requirements"],
    ["install", "Install"],
    ["verify", "Verify"],
    ["versions", "Versions and update"],
    ["app", "App and CLI versions"],
    ["developers", "Developers"],
    ["other", "Homebrew and Linux"],
    ["uninstall", "Uninstall"],
  ],
  style: "pre code{user-select:all}",
  body: `<h2 id="what">What the CLI is</h2>
<p><code>kith</code> creates, runs and shares your agents. Each agent is a folder and a git repo on your computer. The command also starts the host that keeps them running. The Mac app talks to that host: it is a way in, not a separate install.</p>

<h2 id="requirements">Requirements</h2>
<ul>
<li>macOS, Apple silicon or Intel.</li>
<li><code>curl</code> and <code>tar</code> (both ship with macOS).</li>
<li><a href="https://claude.com/claude-code">Claude Code</a>, Codex or pi, installed and signed in, to run agents. Claude Code is the default, and runs for people you invite use it.</li>
</ul>

<h2 id="install">Install</h2>
<pre><code>${SHORT}</code></pre>
<p>That address redirects to the script on GitHub, so this does the same thing:</p>
<pre><code>curl -fsSL ${RAW} | sh</code></pre>
<p>The script downloads the release for your Mac, checks it against the release's <code>SHA256SUMS</code>, installs it, then adds the Claude Code plugin and runs <code>kith init</code>.</p>

<h2 id="verify">Verify</h2>
<pre><code>kith --version
kith doctor</code></pre>
<p><code>kith doctor</code> checks your setup and shows the CLI version and the kernel path it is running from.</p>

<h2 id="versions">Versions and update</h2>
<p>Each release goes into <code>~/.kith/cli/&lt;version&gt;/</code>. <code>~/.kith/cli/current</code> points at the one in use, and <code>~/.local/bin/kith</code> goes through <code>current</code>. Older versions stay side by side (the last three are kept).</p>
<p>To pick a version, set <code>KITH_VERSION</code> before the install line:</p>
<pre><code>curl -fsSL https://kithbot.app/install.sh | KITH_VERSION=1.2.3 sh</code></pre>
<p>To move to the newest release:</p>
<pre><code>kith update self</code></pre>

<h2 id="app">App and CLI versions</h2>
<p>The Mac app requires a CLI version. When you update the app, it installs or updates the CLI for you, so the two stay in step. It never replaces a newer CLI. Mac app downloads are on the <a href="/download">download page</a>.</p>

<h2 id="developers">Developers</h2>
<p>To run Kith from a checkout, point the CLI at its kernel. <code>kith doctor</code> shows the path in use:</p>
<pre><code>KITH_KERNEL=&lt;checkout&gt;/kernel kith doctor</code></pre>
<p>From a checkout, <code>./install.sh --dev</code> links it for you.</p>

<h2 id="other">Homebrew and Linux</h2>
<pre><code>brew install Ying-Kai-Liao/kith/kith</code></pre>
<p>After a Homebrew install, run the install line above once to set up your folder and the plugin.</p>
<p>A Linux VM that only joins someone else's computer uses <code>scripts/install-guest.sh</code>; see <a href="${DOCS}">docs/install.md</a>.</p>

<h2 id="uninstall">Uninstall</h2>
<p>Remove <code>~/.local/bin/kith</code> and <code>~/.kith/cli</code>. Your agents in <code>~/kith</code> and your state in <code>~/.kith</code> stay unless you delete them.</p>
<p>The <a href="${DOCS}">full install notes</a> are on GitHub.</p>`,
  next: [
    ["/download", "The app", "Download Kith"],
    ["/docs", "Next", "Docs"],
  ],
};

export async function installPage(_c: Ctx): Promise<Response> {
  return sitePage(PAGE);
}
