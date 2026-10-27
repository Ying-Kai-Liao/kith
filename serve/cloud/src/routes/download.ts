// The download page at https://kithbot.app/download and its one-click links under /download/*.
// People see and share kithbot.app links; each one 302s to GitHub's latest-release redirect for a fixed asset
// name (no version in it), so nothing here needs a redeploy for a new release. The page works without
// JavaScript; its script only picks the main button.

import type { Ctx } from "../api";

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
var os=/Android|iPhone|iPad|iPod/i.test(p)?"":/Mac/i.test(p)?"mac":/Linux|X11/i.test(p)?"linux":"";
if(!os){document.getElementById("unavailable").hidden=false;return;}
document.getElementById("main").hidden=false;
document.getElementById("main-"+os).hidden=false;
})();`;

const STYLE =
  "body{font:16px/1.5 system-ui,sans-serif;margin:0;background:#fff;color:#111}" +
  "main{max-width:34rem;margin:0 auto;padding:2.5rem 1.5rem}" +
  "h1{margin:0 0 .25rem}h2{font-size:1.1rem;margin:2rem 0 .5rem}" +
  "a.big{display:inline-block;padding:.75rem 1.5rem;border-radius:.6rem;background:#111;color:#fff;text-decoration:none;font-weight:600}" +
  "a{color:inherit}ul{padding-left:1.2rem;margin:.25rem 0}" +
  "code{display:block;padding:.75rem;border-radius:.5rem;background:#f1f1f1;overflow-x:auto;white-space:pre;font-size:.85rem}" +
  "p,.sub{color:#555}[hidden]{display:none}" +
  "@media(prefers-color-scheme:dark){body{background:#111;color:#eee}a.big{background:#eee;color:#111}code{background:#222}p,.sub{color:#aaa}}";

let scriptHash: Promise<string> | undefined;
const hashOfScript = () =>
  (scriptHash ??= crypto.subtle.digest("SHA-256", new TextEncoder().encode(DOWNLOAD_SCRIPT)).then(
    (d) => "sha256-" + btoa(String.fromCharCode(...new Uint8Array(d))),
  ));

const link = (key: string, label: string) => `<li><a href="/download/${key}">${label}</a></li>`;

const HTML = `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>Download Kith</title><style>${STYLE}</style></head>
<body><main>
<h1>Download Kith</h1>
<p class="sub">The latest desktop app.</p>
<div id="main" hidden>
<a id="main-mac" class="big" href="/download/mac" hidden>Download for macOS</a>
<a id="main-linux" class="big" href="/download/linux" hidden>Download for Linux</a>
</div>
<p id="unavailable" hidden>Kith isn't available for your device yet. Pick a download below if you're on a Mac or Linux computer.</p>
<h2>macOS</h2>
<p>Runs on Apple silicon and Intel Macs.</p>
<ul>
${link("mac", "Disk image (.dmg), recommended")}
${link("mac/zip", "Zip (.zip)")}
</ul>
<h2>Linux (x64)</h2>
<ul>
${link("linux", "AppImage, recommended")}
${link("linux/deb", "Debian/Ubuntu (.deb)")}
${link("linux/tar.gz", "Archive (.tar.gz)")}
</ul>
<h2>Command line</h2>
<code>${INSTALL}</code>
<p><a href="${ALL_RELEASES}">All releases</a></p>
</main><script>${DOWNLOAD_SCRIPT}</script></body></html>`;

export async function downloadPage(_c: Ctx): Promise<Response> {
  const hash = await hashOfScript();
  return new Response(HTML, {
    headers: {
      "Content-Type": "text/html; charset=utf-8",
      // Static page: a short public cache is enough.
      "Cache-Control": "public, max-age=300",
      "X-Content-Type-Options": "nosniff",
      "Content-Security-Policy": `default-src 'none'; script-src '${hash}'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'`,
    },
  });
}
