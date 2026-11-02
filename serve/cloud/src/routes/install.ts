// The CLI installation guide at https://kithbot.app/install, and https://kithbot.app/install.sh, which 302s to the
// install script on GitHub so `curl -fsSL https://kithbot.app/install.sh | sh` works. The page is static and has no
// script (the command is select-on-click), so its CSP allows none. Fonts are the landing page's own (site/fonts).

import type { Ctx } from "../api";

const RAW = "https://raw.githubusercontent.com/Ying-Kai-Liao/kith/main/scripts/install.sh";
const SHORT = "curl -fsSL https://kithbot.app/install.sh | sh";

export async function installScript(_c: Ctx): Promise<Response> {
  // No caching, so a moved script takes effect right away.
  return new Response(null, { status: 302, headers: { Location: RAW, "Cache-Control": "no-store" } });
}

const STYLE = `
@font-face{font-family:Inter;font-style:normal;font-weight:400 600;font-display:swap;src:url(/fonts/inter-latin.woff2) format("woff2")}
@font-face{font-family:Nunito;font-style:normal;font-weight:800;font-display:swap;src:url(/fonts/nunito-800-latin.woff2) format("woff2")}
@font-face{font-family:"JetBrains Mono";font-style:normal;font-weight:500;font-display:swap;src:url(/fonts/jetbrains-mono-500-latin.woff2) format("woff2")}
:root{--bg:#FFFFFF;--fg:#14161B;--muted:#5F6472;--soft:#4B5160;--line:#E6E7EE;--panel:#F5F5FA;--focus:#6A45F2;
  --code-bg:#14161B;--code-fg:#E9EAF0;
  --display:Nunito,"SF Pro Rounded",ui-rounded,system-ui,sans-serif;--body:Inter,system-ui,-apple-system,"Segoe UI",sans-serif;
  --mono:"JetBrains Mono",ui-monospace,Menlo,monospace;color-scheme:light}
@media (prefers-color-scheme:dark){:root{--bg:#14161B;--fg:#F2F3F7;--muted:#A3A8B6;--soft:#C3C7D2;--line:#2A2D36;--panel:#1C1F27;
  --focus:#B49CFF;--code-bg:#0D0E12;color-scheme:dark}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--fg);font:400 16px/1.55 var(--body);-webkit-font-smoothing:antialiased;
  padding:0 max(16px,env(safe-area-inset-right,0px)) 0 max(16px,env(safe-area-inset-left,0px));overflow-x:hidden}
a{color:inherit;text-underline-offset:3px;text-decoration-thickness:1px}
:focus-visible{outline:2px solid var(--focus);outline-offset:3px;border-radius:6px}
main{max-width:44rem;margin:0 auto;padding:40px 0 64px}
nav{display:flex;gap:20px;align-items:center;font-size:14px;margin-bottom:40px}
nav .brand{font:800 22px var(--display);letter-spacing:-.03em;text-decoration:none;margin-right:auto}
h1{font:800 clamp(32px,6vw,46px)/1.1 var(--display);letter-spacing:-.03em;margin:0 0 12px}
h2{font:800 22px/1.2 var(--display);letter-spacing:-.02em;margin:40px 0 8px}
p,ul{margin:0 0 12px;color:var(--soft)}ul{padding-left:1.2rem}li{margin-bottom:4px}
.lede{font-size:18px}
code{font:500 .9em var(--mono);background:var(--panel);padding:1px 6px;border-radius:6px;overflow-wrap:anywhere}
pre{margin:8px 0 12px;padding:14px 16px;border-radius:14px;background:var(--code-bg);color:var(--code-fg);overflow-x:auto}
pre code{background:none;padding:0;font-size:13px;white-space:pre;user-select:all;overflow-wrap:normal}
footer{border-top:1px solid var(--line);margin-top:48px;padding-top:16px;font-size:14px;color:var(--muted)}
`;

const HTML = `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover">
<title>Install Kith</title>
<meta name="description" content="Install the kith command line tool on your Mac. The Mac and iPhone apps are clients of it.">
<link rel="icon" href="/img/mark.png">
<style>${STYLE}</style></head>
<body><main>
<nav><a class="brand" href="/">kith</a><a href="/download">Download</a><a href="https://github.com/Ying-Kai-Liao/kith">GitHub</a></nav>
<h1>Install Kith</h1>
<p class="lede">Kith is CLI first. The <code>kith</code> command is the one thing you install; the Mac and iPhone apps are clients that use it.</p>

<h2>What the CLI is</h2>
<p><code>kith</code> creates, runs and shares your agents. Each agent is a folder and a git repo on your computer. The command also starts the host that keeps them running. The Mac app and the iPhone app talk to that host: they are ways in, not separate installs.</p>

<h2>Requirements</h2>
<ul>
<li>macOS, Apple silicon or Intel.</li>
<li><code>curl</code> and <code>tar</code> (both ship with macOS).</li>
<li><a href="https://claude.com/claude-code">Claude Code</a>, to run agents.</li>
</ul>

<h2>Install</h2>
<pre><code>${SHORT}</code></pre>
<p>That address redirects to the script on GitHub, so this does the same thing:</p>
<pre><code>curl -fsSL ${RAW} | sh</code></pre>

<h2>Verify</h2>
<pre><code>kith --version
kith doctor</code></pre>
<p><code>kith doctor</code> checks your setup and shows the CLI version and the kernel path it is running from.</p>

<h2>Versions and update</h2>
<p>Each release goes into <code>~/.kith/cli/&lt;version&gt;/</code>. <code>~/.kith/cli/current</code> points at the one in use, and <code>~/.local/bin/kith</code> goes through <code>current</code>. Older versions stay side by side (the last few are kept).</p>
<p>To pick a version, set <code>KITH_VERSION</code> before the install line:</p>
<pre><code>curl -fsSL https://kithbot.app/install.sh | KITH_VERSION=1.2.3 sh</code></pre>
<p>To move to the newest release:</p>
<pre><code>kith update</code></pre>

<h2>App and CLI versions</h2>
<p>The Mac app requires a CLI version. When you update the app, it installs or updates the CLI for you, so the two stay in step. The iPhone app pairs with a Mac and needs no CLI of its own. Mac app downloads are on the <a href="/download">download page</a>.</p>

<h2>Developers</h2>
<p>To run Kith from a checkout, point the CLI at its kernel. <code>kith doctor</code> shows the path in use:</p>
<pre><code>KITH_KERNEL=&lt;checkout&gt;/kernel kith doctor</code></pre>
<p>From a checkout, <code>./install.sh --dev</code> links it for you.</p>

<h2>Homebrew and Linux</h2>
<pre><code>brew install Ying-Kai-Liao/kith/kith</code></pre>
<p>A Linux VM that only joins someone else's computer uses <code>scripts/install-guest.sh</code>; see <a href="https://github.com/Ying-Kai-Liao/kith/blob/main/docs/install.md">docs/install.md</a>.</p>

<h2>Uninstall</h2>
<p>Remove <code>~/.local/bin/kith</code> and <code>~/.kith/cli</code>. Your agents in <code>~/kith</code> and your state in <code>~/.kith</code> stay unless you delete them.</p>

<footer>Kith is open source. <a href="https://github.com/Ying-Kai-Liao/kith/blob/main/docs/install.md">Full install notes</a></footer>
</main></body></html>`;

export async function installPage(_c: Ctx): Promise<Response> {
  return new Response(HTML, {
    headers: {
      "Content-Type": "text/html; charset=utf-8",
      "Cache-Control": "public, max-age=300",
      "X-Content-Type-Options": "nosniff",
      "Content-Security-Policy":
        "default-src 'none'; style-src 'unsafe-inline'; img-src 'self'; font-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
    },
  });
}
