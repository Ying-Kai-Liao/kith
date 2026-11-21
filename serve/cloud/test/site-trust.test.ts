// The trust pages of the site: /sharing, /privacy and /security. Static pages in the shared frame (routes/site.ts).

import { SELF } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { ORIGIN } from "./helpers";

const get = (path: string) => SELF.fetch(`${ORIGIN}${path}`, { redirect: "manual" });
const REPO = "https://github.com/Ying-Kai-Liao/kith";

/** The page, after the checks every site page passes. */
async function page(path: string, title: string): Promise<string> {
  const res = await get(path);
  expect(res.status).toBe(200);
  expect(res.headers.get("content-type")).toBe("text/html; charset=utf-8");
  expect(res.headers.get("x-content-type-options")).toBe("nosniff");
  const csp = res.headers.get("content-security-policy")!;
  expect(csp).toContain("default-src 'none'");
  expect(csp).not.toContain("script-src");
  const html = await res.text();
  expect(html).toContain(`<title>${title}</title>`);
  expect(html).toContain(`<link rel="canonical" href="https://kithbot.app${path}">`);
  expect(html).not.toMatch(/<script/i);
  // The voice rules: no em or en dash, no exclamation mark in the text, none of the banned words.
  expect(html).not.toMatch(/[–—]/);
  const text = html.replace(/<style>[\s\S]*?<\/style>/g, "").replace(/<[^>]+>/g, " ");
  expect(text).not.toContain("!");
  expect(text).not.toMatch(/\bAI\b/);
  expect(text).not.toMatch(/platform/i);
  expect(text).not.toMatch(/\b(Cursor|Muse|TestFlight|Linux)\b/);
  // Every contents entry points at a heading on the page, and every description is filled in.
  const ids = [...html.matchAll(/<nav class="toc"[\s\S]*?<\/nav>/g)].flatMap((m) => [...m[0].matchAll(/href="#([^"]+)"/g)].map((x) => x[1]));
  for (const id of ids) expect(html).toContain(`<h2 id="${id}">`);
  expect(html).not.toContain('<meta name="description" content="">');
  // No outside resources: the CSP would block them anyway.
  expect(html).not.toMatch(/(src|url\()\s*=?\s*["']?https?:/i);
  return html;
}

describe("GET /sharing", () => {
  it("explains sharing with the product's real numbers and commands", async () => {
    const html = await page("/sharing", "Share an agent · Kith");
    expect(html).toContain('aria-current="page">Sharing<');
    expect(html).toContain("expires after 24 hours");
    expect(html).toContain("$20 a calendar month");
    expect(html).toContain("kith invite add Rex --agent reading");
    expect(html).toContain("kith invite revoke");
    expect(html).toContain("<th>Guest</th><th>Member</th>");
    expect(html).toContain("memory/people/&lt;id&gt;/");
    expect(html).toContain("Restore access");
    expect(html).toContain("runs on Claude Code");
    expect(html).toContain('href="/privacy"');
    expect(html).toContain('href="/agents"');
    for (const id of ["what", "invite", "they", "roles", "limits", "reach", "bots", "remove", "offline", "engine"]) {
      expect(html).toContain(`<h2 id="${id}">`);
    }
  });
});

describe("GET /privacy", () => {
  it("restates PRIVACY.md and links it as the authoritative text", async () => {
    const html = await page("/privacy", "Privacy · Kith");
    expect(html).toContain(`${REPO}/blob/main/PRIVACY.md`);
    expect(html).toContain("This is on unless you turn it off.");
    expect(html).toContain("Settings &gt; Privacy");
    expect(html).toContain("us.i.posthog.com");
    expect(html).toContain("~/.kith/serve-main/serve.db");
    expect(html).toContain("The database is not encrypted.");
    expect(html).toContain("for up to 24 hours");
    expect(html).toContain("for up to 72 hours");
    expect(html).toContain("deleted after 30 days");
    expect(html).toContain("The account record, including the email address, is kept.");
    expect(html).toContain("Anthropic, through Claude Code.");
    // PRIVACY.md names no policy for the other engines, so the page names none either.
    expect(html).not.toMatch(/Codex|\bpi\b/);
  });
});

describe("GET /security", () => {
  it("restates SECURITY.md: how to report, the model, the known gaps", async () => {
    const html = await page("/security", "Security · Kith");
    expect(html).toContain(`${REPO}/blob/main/SECURITY.md`);
    expect(html).toContain(`${REPO}/security`);
    expect(html).toContain("Report a vulnerability");
    expect(html).toContain("Please don't open a public issue.");
    expect(html).toContain("kith --version");
    expect(html).toContain("X25519");
    expect(html).toContain("ChaCha20-Poly1305");
    expect(html).toContain("7FF80CDEC1769B21");
    expect(html).toContain("The Mac app is not signed or notarized");
    expect(html).toContain("Chats are stored unencrypted");
    expect(html).toContain('href="/privacy#delete"');
  });
});

describe("the trust pages link each other", () => {
  it("the footer of each names privacy and security", async () => {
    for (const path of ["/sharing", "/privacy", "/security"]) {
      const html = await (await get(path)).text();
      expect(html).toContain('<a href="/privacy">Privacy</a>');
      expect(html).toContain('<a href="/security">Security</a>');
    }
  });
});
