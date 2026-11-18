// The builder pages of the site: /docs (Get started) and /agents (The agent format).

import { SELF } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { ORIGIN } from "./helpers";

const get = (path: string) => SELF.fetch(`${ORIGIN}${path}`, { redirect: "manual" });

/** The page's own words: tags and the shared style dropped. */
const text = (html: string) => html.replace(/<style[\s\S]*?<\/style>/g, "").replace(/<[^>]+>/g, " ");

async function page(path: string) {
  const res = await get(path);
  expect(res.status).toBe(200);
  expect(res.headers.get("content-type")).toBe("text/html; charset=utf-8");
  expect(res.headers.get("x-content-type-options")).toBe("nosniff");
  const csp = res.headers.get("content-security-policy")!;
  expect(csp).toContain("default-src 'none'");
  expect(csp).not.toContain("script-src");
  const html = await res.text();
  expect(html).not.toMatch(/<script/i);
  return html;
}

/** Every id the contents list links to is a section heading on the page. */
function tocResolves(html: string) {
  const toc = /<nav class="toc"[\s\S]*?<\/nav>/.exec(html)?.[0] ?? "";
  const ids = [...toc.matchAll(/href="#([^"]+)"/g)].map((m) => m[1]);
  expect(ids.length).toBeGreaterThan(5);
  for (const id of ids) expect(html).toContain(`<h2 id="${id}">`);
}

describe("GET /docs", () => {
  it("serves the guide with its commands and a script-free CSP", async () => {
    const html = await page("/docs");
    expect(html).toContain("<title>Get started · Kith</title>");
    expect(html).toContain("<h1>Get started</h1>");
    expect(html).toContain('<a href="/docs" aria-current="page">');
    for (const s of [
      "curl -fsSL https://kithbot.app/install.sh | sh",
      'kith new reading "tracks what I read"',
      "kith ask reading -c",
      "kith reading skill add pdf",
      "needs: [notion]",
      "kith auth grant reading notion notion/library",
      "kith keys add notion --profile library --for reading",
      "kith schedule --install",
      'kith schedule add reading "0 8 * * 5"',
      "kith auth grant serve telegram telegram/home",
      "/link CODE",
      "kith cost agents",
      "kith history reading",
      "kith restore reading COMMIT",
      'href="/download"',
      'href="/install"',
    ])
      expect(html).toContain(s);
    tocResolves(html);
    expect(html).toMatch(/<div class="next"><a href="\/agents">.*<a href="\/sharing">/);
  });
});

describe("GET /agents", () => {
  it("serves the format reference with every frontmatter field", async () => {
    const html = await page("/agents");
    expect(html).toContain("<title>The agent format · Kith</title>");
    expect(html).toContain("<h1>The agent format</h1>");
    expect(html).toContain('<a href="/agents" aria-current="page">');
    for (const field of ["name", "description", "model", "effort", "harness", "display_name", "memory_cap"])
      expect(html).toContain(`<dt>${field}</dt>`);
    for (const s of [
      "agents/reading/",
      "capabilities.yaml",
      "role/skills/learned/",
      "{{AGENT_DIR}}",
      "kith share reading --public --fresh",
      "kith install owner/repo",
      "kith history reading",
      "kith restore reading COMMIT",
      "kith rollback reading",
      "kith reading set harness codex",
      "kith ask reading -H pi",
      "memory/people/&lt;id&gt;/",
    ])
      expect(html).toContain(s);
    tocResolves(html);
    expect(html).toMatch(/<div class="next"><a href="\/docs">.*<a href="\/sharing">/);
  });
});

describe("the voice of both pages", () => {
  it("has no dashes as punctuation, no exclamation marks and none of the banned words", async () => {
    for (const path of ["/docs", "/agents"]) {
      const t = text(await page(path));
      expect(t, path).not.toMatch(/[–—]/);
      expect(t, path).not.toMatch(/\bAI\b/);
      expect(t, path).not.toMatch(/platform/i);
      // As punctuation only: Slack's commands start with one (`!link CODE`).
      expect(t, path).not.toMatch(/[\w.)"']!/);
      expect(t, path).not.toMatch(/\b(Cursor|Muse)\b/);
    }
  });
});
