// The shared frame of the site's inner pages (/docs, /agents, /sharing, /privacy, /security, /download, /install):
// the home page's tokens, type and sky, one nav and one footer. A page gives its title, lede, sections and body;
// `sitePage` returns the answer with a locked-down CSP. Pages are static; one that needs a script passes it and the
// CSP names its hash. Images and fonts are the site's own (site/img, site/fonts).

const GITHUB = "https://github.com/Ying-Kai-Liao/kith";

export const SITE_STYLE = `
@font-face{font-family:Inter;font-style:normal;font-weight:400 600;font-display:swap;src:url(/fonts/inter-latin.woff2) format("woff2")}
@font-face{font-family:Nunito;font-style:normal;font-weight:800;font-display:swap;src:url(/fonts/nunito-800-latin.woff2) format("woff2")}
@font-face{font-family:"JetBrains Mono";font-style:normal;font-weight:500;font-display:swap;src:url(/fonts/jetbrains-mono-500-latin.woff2) format("woff2")}
:root{--bg:#FFFFFF;--fg:#14161B;--muted:#5F6472;--soft:#4B5160;--line:#E6E7EE;--panel:#F5F5FA;--card:#FFFFFF;
  --btn:#14161B;--btn-fg:#FFFFFF;--ghost:rgba(255,255,255,.72);--focus:#6A45F2;--accent:#6A45F2;
  --code-bg:#14161B;--code-fg:#E9EAF0;--code-dim:#8D93A3;--sky:url(/img/sky-day.jpg);
  --display:Nunito,"SF Pro Rounded",ui-rounded,system-ui,sans-serif;--body:Inter,system-ui,-apple-system,"Segoe UI",sans-serif;
  --mono:"JetBrains Mono",ui-monospace,Menlo,monospace;color-scheme:light}
@media (prefers-color-scheme:dark){:root{--bg:#14161B;--fg:#F2F3F7;--muted:#A3A8B6;--soft:#C3C7D2;--line:#2A2D36;--panel:#1C1F27;--card:#1C1F27;
  --btn:#F2F3F7;--btn-fg:#14161B;--ghost:rgba(28,31,39,.7);--focus:#B49CFF;--accent:#B49CFF;
  --code-bg:#0D0E12;--code-dim:#7D8393;--sky:url(/img/sky-night.jpg);color-scheme:dark}}
*{box-sizing:border-box}
[hidden]{display:none!important}
html{scroll-behavior:smooth;scroll-padding-top:24px}
body{margin:0;background:var(--bg);color:var(--fg);font:400 16px/1.6 var(--body);-webkit-font-smoothing:antialiased;position:relative;overflow-x:hidden;
  padding:0 max(16px,env(safe-area-inset-right,0px)) 0 max(16px,env(safe-area-inset-left,0px))}
::selection{background:#C9B8FF;color:#14161B}
a{color:inherit;text-underline-offset:3px;text-decoration-thickness:1px}
:focus-visible{outline:2px solid var(--focus);outline-offset:3px;border-radius:6px}
h1,h2,h3,p,ul,ol,pre,dl,dd,table{margin:0}
.sky{position:absolute;inset:0 0 auto 0;height:340px;background:var(--sky) top center/cover no-repeat;pointer-events:none}
.sky::after{content:"";position:absolute;inset:0;background:linear-gradient(transparent 20%,var(--bg))}
.col{max-width:1120px;margin:0 auto;position:relative}
.btn{display:inline-flex;align-items:center;justify-content:center;gap:8px;min-height:44px;padding:0 20px;border-radius:22px;border:0;
  background:var(--btn);color:var(--btn-fg);font:600 15px var(--body);text-decoration:none;cursor:pointer}
.btn.sm{min-height:36px;padding:0 16px;font-size:14px}
.btn.ghost{background:var(--ghost);color:var(--fg);box-shadow:inset 0 0 0 1px color-mix(in srgb,var(--fg) 14%,transparent)}
nav.top{display:flex;align-items:center;gap:28px;padding-block:20px}
.brand{display:flex;align-items:center;gap:8px;font:800 24px/1 var(--display);letter-spacing:-.03em;text-decoration:none}
.brand img{width:32px;height:32px}
nav.top .links{display:flex;gap:26px;font:500 15px var(--body);color:var(--soft);margin-left:12px}
nav.top .links a{text-decoration:none}
nav.top .links a:hover,nav.top .links a[aria-current]{color:var(--fg)}
nav.top .links a[aria-current]{font-weight:600}
nav.top .btn{margin-left:auto}

/* page head, then sections with a contents list beside them on wide screens */
.head{padding-block:clamp(40px,7vw,80px) clamp(32px,5vw,56px);display:flex;flex-direction:column;gap:14px}
.head .kicker{font:600 14px var(--body);color:var(--muted)}
h1{font:800 clamp(34px,5vw,52px)/1.06 var(--display);letter-spacing:-.035em;text-wrap:balance;max-width:16em}
.lede{font-size:clamp(17px,1.6vw,19px);color:var(--soft);max-width:38em;text-wrap:pretty}
.acts{display:flex;gap:12px;flex-wrap:wrap;margin-top:8px}
.frame{display:grid;grid-template-columns:200px minmax(0,720px);gap:64px;align-items:start}
.frame.plain{grid-template-columns:minmax(0,720px)}
.toc{position:sticky;top:24px;display:flex;flex-direction:column;gap:2px;font-size:14px;color:var(--muted)}
.toc b{font:600 13px var(--body);color:var(--fg);margin-bottom:8px}
.toc a{text-decoration:none;padding:4px 0}
.toc a:hover{color:var(--fg)}
.prose{display:flex;flex-direction:column;gap:14px;min-width:0}
.prose h2{font:800 clamp(24px,2.6vw,30px)/1.15 var(--display);letter-spacing:-.025em;margin-top:40px;padding-top:32px;border-top:1px solid var(--line)}
.prose h2:first-child{margin-top:0;padding-top:0;border-top:0}
.prose h3{font:600 17px/1.3 var(--body);margin-top:14px}
.prose p,.prose li,.prose dd{color:var(--soft);text-wrap:pretty}
.prose ul,.prose ol{padding-left:1.25rem;display:flex;flex-direction:column;gap:6px}
.prose strong{color:var(--fg);font-weight:600}
code{font:500 .9em var(--mono);background:var(--panel);padding:1px 6px;border-radius:6px;overflow-wrap:anywhere}
pre{padding:16px 18px;border-radius:16px;background:var(--code-bg);color:var(--code-fg);overflow-x:auto;font:500 13.5px/1.75 var(--mono)}
pre code{background:none;padding:0;font-size:inherit;white-space:pre;overflow-wrap:normal}
pre .c{color:var(--code-dim)}
.note{border-radius:16px;background:var(--panel);padding:16px 18px;color:var(--soft)}
.note b{color:var(--fg)}
.cards{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:16px;list-style:none;padding:0!important}
.cards li{border-radius:16px;background:var(--panel);padding:18px 20px;display:flex;flex-direction:column;gap:4px}
.cards h3{margin:0}
.cards p{font-size:15px}
.wrap{overflow-x:auto}
table{border-collapse:collapse;width:100%;font-size:15px}
th{text-align:left;font:600 14px var(--body);padding:10px 16px 10px 0;border-bottom:1px solid var(--fg)}
td{padding:11px 16px 11px 0;border-bottom:1px solid var(--line);color:var(--soft);vertical-align:top}
td:first-child{color:var(--fg)}
td code,dt code{overflow-wrap:normal;white-space:nowrap}
dl{display:grid;grid-template-columns:minmax(120px,max-content) minmax(0,1fr)}
dt,dd{padding:11px 0;border-bottom:1px solid var(--line)}
dt{padding-right:24px}
dt{font:500 14px var(--mono);color:var(--fg)}
.next{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:16px;margin-top:56px}
.next a{border-radius:16px;box-shadow:inset 0 0 0 1px var(--line);padding:18px 20px;text-decoration:none;display:flex;flex-direction:column;gap:2px}
.next a:hover{background:var(--panel)}
.next span{font-size:13px;color:var(--muted)}
.next b{font:800 18px var(--display)}
footer{border-top:1px solid var(--line);margin-top:clamp(64px,9vw,104px);padding-block:40px 56px;display:grid;grid-template-columns:minmax(0,1.5fr) repeat(3,minmax(0,1fr));gap:32px;font-size:14px;color:var(--muted)}
footer .brand{font-size:18px;color:var(--fg);margin-bottom:10px}
footer .brand img{width:22px;height:22px}
footer h3{font:600 14px var(--body);color:var(--fg);margin-bottom:10px}
footer ul{list-style:none;padding:0;display:flex;flex-direction:column;gap:8px}
footer a{text-decoration:none}
footer a:hover{color:var(--fg)}
@media (max-width:900px){
  .frame{grid-template-columns:minmax(0,1fr);gap:24px}
  .toc{position:static;flex-direction:row;flex-wrap:wrap;gap:4px 16px;padding-bottom:20px;border-bottom:1px solid var(--line)}
  .toc b{display:none}
}
@media (max-width:720px){
  nav.top .links{display:none}
  .sky{height:260px}
  .cards,.next{grid-template-columns:1fr}
  dl{grid-template-columns:1fr}
  dt{border-bottom:0;padding-bottom:0;padding-right:0}
  td code,dt code{white-space:normal;overflow-wrap:anywhere}
  dd{padding-top:4px}
  footer{grid-template-columns:repeat(3,minmax(0,1fr));gap:28px 16px}
  footer .about{grid-column:1/-1}
}
`;

/** The nav's own pages, in order; `path` marks the current one. */
const LINKS: [string, string][] = [
  ["/docs", "Docs"],
  ["/agents", "Agents"],
  ["/sharing", "Sharing"],
  ["/install", "Install"],
];

export const SITE_FOOTER = `<footer>
  <div class="about"><a class="brand" href="/"><img src="/img/mark.png" width="22" height="22" alt="">kith</a><p>Kith is open source. Agents run on macOS.</p></div>
  <div><h3>Product</h3><ul><li><a href="/#start">How it works</a></li><li><a href="/sharing">Sharing</a></li><li><a href="/download">Download</a></li><li><a href="/install">Install</a></li><li><a href="/join/">Sign in</a></li></ul></div>
  <div><h3>Builders</h3><ul><li><a href="/docs">Docs</a></li><li><a href="/agents">Agent format</a></li><li><a href="${GITHUB}">GitHub</a></li></ul></div>
  <div><h3>Trust</h3><ul><li><a href="/privacy">Privacy</a></li><li><a href="/security">Security</a></li><li><a href="${GITHUB}/blob/main/LICENSE">License</a></li></ul></div>
</footer>`;

export interface SitePage {
  /** The route, e.g. "/docs": marks the nav and the canonical link. */
  path: string;
  /** The <title>, e.g. "Docs · Kith". */
  title: string;
  description: string;
  /** A small line above the heading, e.g. "Docs". Optional. */
  kicker?: string;
  h1: string;
  /** One or two sentences under the heading; HTML. */
  lede: string;
  /** Buttons under the lede; HTML. Optional. */
  acts?: string;
  /** The sections for the contents list: [id, label]. Each id is an <h2 id> in `body`. Omit for a short page. */
  toc?: [string, string][];
  /** The page's sections; HTML that goes inside <div class="prose">. */
  body: string;
  /** "Next" cards at the foot: [href, small label, title]. Optional, at most two. */
  next?: [string, string, string][];
  /** Extra CSS for this page only. Optional. */
  style?: string;
  /** One inline script; the CSP names its hash. Optional: most pages have none. */
  script?: string;
}

/** The whole document for a page. Exported so a test can read it without the Worker. */
export function siteHtml(p: SitePage): string {
  const links = LINKS.map(([href, label]) => `<a href="${href}"${href === p.path ? ' aria-current="page"' : ""}>${label}</a>`).join("");
  const toc = p.toc?.length
    ? `<nav class="toc" aria-label="On this page"><b>On this page</b>${p.toc.map(([id, label]) => `<a href="#${id}">${label}</a>`).join("")}</nav>`
    : "";
  const next = p.next?.length
    ? `<div class="next">${p.next.map(([href, small, title]) => `<a href="${href}"><span>${small}</span><b>${title}</b></a>`).join("")}</div>`
    : "";
  return `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover">
<title>${p.title}</title>
<meta name="description" content="${p.description}">
<link rel="canonical" href="https://kithbot.app${p.path}">
<link rel="icon" href="/img/mark.png">
<style>${SITE_STYLE}${p.style ?? ""}</style></head>
<body>
<div class="sky" aria-hidden="true"></div>
<div class="col">
<nav class="top" aria-label="Main">
  <a class="brand" href="/"><img src="/img/mark.png" width="32" height="32" alt="">kith</a>
  <div class="links">${links}<a href="${GITHUB}">GitHub</a></div>
  <a class="btn sm" href="/download">Download</a>
</nav>
<main>
<header class="head">
  ${p.kicker ? `<p class="kicker">${p.kicker}</p>` : ""}<h1>${p.h1}</h1>
  <p class="lede">${p.lede}</p>${p.acts ? `\n  <div class="acts">${p.acts}</div>` : ""}
</header>
<div class="frame${toc ? "" : " plain"}">
${toc}<div><div class="prose">
${p.body}
</div>${next}</div>
</div>
</main>
${SITE_FOOTER}
</div>${p.script ? `\n<script>${p.script}</script>` : ""}
</body></html>`;
}

const hashes = new Map<string, Promise<string>>();
const hashOf = (script: string) => {
  let h = hashes.get(script);
  if (!h) {
    h = crypto.subtle
      .digest("SHA-256", new TextEncoder().encode(script))
      .then((d) => "sha256-" + btoa(String.fromCharCode(...new Uint8Array(d))));
    hashes.set(script, h);
  }
  return h;
};

/** The answer for a page: static HTML, a short public cache, and a CSP that allows only the site's own files. */
export async function sitePage(p: SitePage): Promise<Response> {
  const script = p.script ? `script-src '${await hashOf(p.script)}'; ` : "";
  return new Response(siteHtml(p), {
    headers: {
      "Content-Type": "text/html; charset=utf-8",
      "Cache-Control": "public, max-age=300",
      "X-Content-Type-Options": "nosniff",
      "Content-Security-Policy": `default-src 'none'; ${script}style-src 'unsafe-inline'; img-src 'self'; font-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'`,
    },
  });
}
