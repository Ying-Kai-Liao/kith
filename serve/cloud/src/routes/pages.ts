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

const STYLE =
  "body{font:16px/1.5 system-ui,sans-serif;margin:0;min-height:100vh;display:flex;align-items:center;justify-content:center;background:#fff;color:#111}" +
  "main{max-width:22rem;padding:1.5rem;text-align:center}" +
  "a{display:inline-block;padding:.75rem 1.5rem;border-radius:.6rem;background:#111;color:#fff;text-decoration:none;font-weight:600}" +
  "p{color:#555}[hidden]{display:none}" +
  "@media(prefers-color-scheme:dark){body{background:#111;color:#eee}a{background:#eee;color:#111}p{color:#aaa}}";

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
<meta name="robots" content="noindex"><title>Open in Kith</title><style>${STYLE}</style></head>
<body data-c="${escapeHtml(cid)}" data-i="${escapeHtml(iid)}"><main>
<h1>You're invited to Kith</h1>
<a id="open" hidden>Open in Kith</a>
<p id="bad" hidden>This invite link is incomplete. Ask for a new one.</p>
<p id="note" hidden>Don't have Kith yet? Ask the person who invited you how to get it.</p>
<noscript><p>Open this link in a browser with JavaScript on.</p></noscript>
</main><script>${INVITE_SCRIPT}</script></body></html>`;
  return new Response(html, {
    headers: {
      "Content-Type": "text/html; charset=utf-8",
      "Cache-Control": "no-store",
      "Referrer-Policy": "no-referrer",
      "X-Content-Type-Options": "nosniff",
      "Content-Security-Policy": `default-src 'none'; script-src '${hash}'; style-src 'unsafe-inline'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'`,
    },
  });
}
