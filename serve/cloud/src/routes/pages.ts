// The open-in-Kith page behind https://kithbot.app/i/<computerId>/<inviteId>#t=<token>.
// The token rides in the URL fragment, which the browser never sends: this route only ever sees the two ids.

import type { Ctx } from "../api";

const ID = /^[A-Za-z0-9_-]{1,64}$/;

/** Static, so one CSP hash covers it; the ids reach it as data attributes, never interpolated into code. */
export const INVITE_SCRIPT = `(function(){
var d=document.body.dataset;
var t=new URLSearchParams(location.hash.slice(1)).get("t");
var a=document.getElementById("open");
if(!t||!/^[A-Za-z0-9_-]{8,512}$/.test(t)){document.getElementById("bad").hidden=false;return;}
var u="kith://invite?c="+encodeURIComponent(d.c)+"&i="+encodeURIComponent(d.i)+"&t="+encodeURIComponent(t);
a.href=u;a.hidden=false;
document.getElementById("note").hidden=false;
location.href=u;
})();`;

const escapeHtml = (s: string) =>
  s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!);

// The site's own type, mark and colours (routes/site.ts), copied here so this page stays on its own tight CSP.
const STYLE = `
@font-face{font-family:Inter;font-style:normal;font-weight:400 600;font-display:swap;src:url(/fonts/inter-latin.woff2) format("woff2")}
@font-face{font-family:Nunito;font-style:normal;font-weight:800;font-display:swap;src:url(/fonts/nunito-800-latin.woff2) format("woff2")}
:root{--bg:#FFFFFF;--fg:#14161B;--muted:#5F6472;--soft:#4B5160;--line:#E6E7EE;--btn:#14161B;--btn-fg:#FFFFFF;--focus:#6A45F2;--sky:url(/img/sky-day.jpg);
  --display:Nunito,"SF Pro Rounded",ui-rounded,system-ui,sans-serif;--body:Inter,system-ui,-apple-system,"Segoe UI",sans-serif;color-scheme:light}
@media (prefers-color-scheme:dark){:root{--bg:#14161B;--fg:#F2F3F7;--muted:#A3A8B6;--soft:#C3C7D2;--line:#2A2D36;--btn:#F2F3F7;--btn-fg:#14161B;--focus:#B49CFF;
  --sky:url(/img/sky-night.jpg);color-scheme:dark}}
*{box-sizing:border-box}
[hidden]{display:none!important}
body{margin:0;min-height:100vh;min-height:100dvh;display:flex;align-items:center;justify-content:center;position:relative;
  background:var(--bg);color:var(--fg);font:400 16px/1.6 var(--body);-webkit-font-smoothing:antialiased;padding:24px 20px}
.sky{position:absolute;inset:0 0 auto 0;height:300px;background:var(--sky) top center/cover no-repeat;pointer-events:none}
.sky::after{content:"";position:absolute;inset:0;background:linear-gradient(transparent 10%,var(--bg))}
main{position:relative;max-width:24rem;text-align:center;display:flex;flex-direction:column;align-items:center;gap:16px}
.brand{display:flex;align-items:center;gap:8px;font:800 24px/1 var(--display);letter-spacing:-.03em;margin-bottom:12px}
.brand img{width:32px;height:32px}
h1{margin:0;font:800 clamp(30px,8vw,38px)/1.1 var(--display);letter-spacing:-.035em;text-wrap:balance}
p{margin:0;color:var(--soft);text-wrap:pretty}
a{display:inline-flex;align-items:center;justify-content:center;min-height:48px;padding:0 26px;margin-top:6px;border-radius:24px;
  background:var(--btn);color:var(--btn-fg);font:600 16px var(--body);text-decoration:none}
:focus-visible{outline:2px solid var(--focus);outline-offset:3px}
.fine{font-size:14px;color:var(--muted);border-top:1px solid var(--line);padding-top:16px;margin-top:8px}
`;

let scriptHash: Promise<string> | undefined;
const hashOfScript = () =>
  (scriptHash ??= crypto.subtle.digest("SHA-256", new TextEncoder().encode(INVITE_SCRIPT)).then(
    (d) => "sha256-" + btoa(String.fromCharCode(...new Uint8Array(d))),
  ));

export async function invitePage(_c: Ctx, [cid, iid]: string[]): Promise<Response> {
  // The route regex already limits these; the check stays so the page never trusts its caller.
  if (!ID.test(cid ?? "") || !ID.test(iid ?? "")) return new Response("Not found", { status: 404 });
  const hash = await hashOfScript();
  const html = `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<meta name="robots" content="noindex"><title>Open in Kith</title><link rel="icon" href="/img/mark.png"><style>${STYLE}</style></head>
<body data-c="${escapeHtml(cid)}" data-i="${escapeHtml(iid)}"><div class="sky" aria-hidden="true"></div><main>
<div class="brand"><img src="/img/mark.png" width="32" height="32" alt="">kith</div>
<h1>You're invited to Kith</h1>
<p>Someone invited you to talk to an agent of theirs. An agent is a helper that runs on their Mac, and Kith is the app where you talk to it.</p>
<a id="open" hidden>Open in Kith</a>
<p id="bad" hidden>This invite link is incomplete. Ask for a new one.</p>
<p id="note" hidden>Don't have Kith yet? Ask the person who invited you how to get it.</p>
<noscript><p>Open this link in a browser with JavaScript on.</p></noscript>
<p class="fine">The link works for 24 hours and for one person. Keep it to yourself until you have used it.</p>
</main><script>${INVITE_SCRIPT}</script></body></html>`;
  return new Response(html, {
    headers: {
      "Content-Type": "text/html; charset=utf-8",
      "Cache-Control": "no-store",
      "Referrer-Policy": "no-referrer",
      "X-Content-Type-Options": "nosniff",
      "Content-Security-Policy": `default-src 'none'; script-src '${hash}'; style-src 'unsafe-inline'; img-src 'self'; font-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'`,
    },
  });
}
