// The download page at https://kithbot.app/download and its one-click links under /download/*.
// People see and share kithbot.app links; each one 302s to GitHub's latest-release redirect for a fixed asset
// name (no version in it), so nothing here needs a redeploy for a new release. The page works without
// JavaScript; its script only picks the main button. It sits in the site's shared frame (routes/site.ts), which
// names the script's hash in the CSP.

import type { Ctx } from "../api";
import { sitePage, type SitePage } from "./site";

const BASE = "https://github.com/Ying-Kai-Liao/kith/releases/latest/download/";
const ALL_RELEASES = "https://github.com/Ying-Kai-Liao/kith/releases/latest";
const INSTALL = "curl -fsSL https://raw.githubusercontent.com/Ying-Kai-Liao/kith/main/scripts/install.sh | sh";

/** kithbot.app/download/<key> -> release asset name. */
export const DOWNLOADS: Record<string, string> = {
  mac: "kith-macos.dmg",
  "mac/zip": "kith-macos.zip",
  linux: "kith-linux-x64.AppImage",
  "linux/appimage": "kith-linux-x64.AppImage",
  "linux/deb": "kith-linux-x64.deb",
  "linux/tar.gz": "kith-linux-x64.tar.gz",
};

export async function downloadRedirect(_c: Ctx, [key]: string[]): Promise<Response> {
  const asset = Object.hasOwn(DOWNLOADS, key ?? "") ? DOWNLOADS[key!] : undefined;
  if (!asset) return new Response("Not found", { status: 404 });
  // No caching, so a new release takes effect right away.
  return new Response(null, { status: 302, headers: { Location: BASE + asset, "Cache-Control": "no-store" } });
}

/** Static, so one CSP hash covers it. Only shows the main button for the visitor's OS; the lists stay as they are. */
export const DOWNLOAD_SCRIPT = `(function(){
var n=navigator,p=((n.userAgentData&&n.userAgentData.platform)||"")+" "+(n.userAgent||"");
var os=/Android|iPhone|iPad|iPod/i.test(p)?"":/Mac/i.test(p)?(n.maxTouchPoints>1?"":"mac"):/Linux|X11/i.test(p)?"linux":"";
if(!os){document.getElementById("unavailable").hidden=false;return;}
document.getElementById("main").hidden=false;
document.getElementById("main-"+os).hidden=false;
})();`;

const link = (key: string, label: string) => `<li><a href="/download/${key}">${label}</a></li>`;

// [hidden] has to beat .btn's display, so only the visitor's button shows: the frame's style has
// `[hidden]{display:none!important}`.
const STYLE = `
.pick{display:flex;flex-direction:column;gap:10px;align-items:flex-start}
.pick p{font-size:14px;color:var(--muted)}
.prose .pick+p+h2{margin-top:26px}
pre code{user-select:all}
`;

export const PAGE: SitePage = {
  path: "/download",
  title: "Download Kith",
  description: "Download the Kith app for macOS, or install the kith command line tool.",
  h1: "Download Kith",
  lede: "The Kith app for your Mac. It carries the <code>kith</code> command and the host that runs your agents.",
  body: `<div id="main" class="pick" hidden>
<div id="main-mac" class="pick" hidden><a class="btn" href="/download/mac">Download for macOS</a><p>Disk image (.dmg) for Apple silicon and Intel Macs.</p></div>
<div id="main-linux" class="pick" hidden><a class="btn" href="/download/linux">Download for Linux</a><p>AppImage for x64. It is a client: your agents run on a Mac.</p></div>
</div>
<p id="unavailable" class="note" hidden>Kith runs on a Mac. Open this page on your Mac, or pick a download below.</p>
<h2 id="macos">macOS</h2>
<p>Runs on Apple silicon and Intel Macs. The app updates the <code>kith</code> command when it updates itself. Claude Code, Codex and pi are separate installs: have at least one signed in.</p>
<ul>
${link("mac", "Disk image (.dmg), recommended")}
${link("mac/zip", "Zip (.zip)")}
</ul>
<h2 id="linux">Linux (x64)</h2>
<p>The Linux app is a client. It does not include the host that runs agents, so use it to reach a Mac that does.</p>
<ul>
${link("linux", "AppImage, recommended")}
${link("linux/deb", "Debian/Ubuntu (.deb)")}
${link("linux/tar.gz", "Archive (.tar.gz)")}
</ul>
<h2 id="cli">Command line</h2>
<p>To install only the <code>kith</code> command on a Mac:</p>
<pre><code>${INSTALL}</code></pre>
<p>The <a href="/install">install guide</a> covers versions, updates and uninstalling. Every build is on GitHub under <a href="${ALL_RELEASES}">all releases</a>.</p>`,
  style: STYLE,
  script: DOWNLOAD_SCRIPT,
};

export async function downloadPage(_c: Ctx): Promise<Response> {
  return sitePage(PAGE);
}
