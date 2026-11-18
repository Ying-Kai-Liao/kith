// The home page at https://kithbot.app/ and its static images and fonts.

import { SELF } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { LANDING_SCRIPT } from "../src/routes/landing";
import { env, ORIGIN } from "./helpers";

const get = (path: string) => SELF.fetch(`${ORIGIN}${path}`, { redirect: "manual" });
// In production the assets router answers these before the Worker runs; SELF in the tests skips that router.
const asset = (path: string) => env.ASSETS.fetch(`${ORIGIN}${path}`);

describe("GET /", () => {
  it("serves the page with a locked-down CSP whose hash matches its one script", async () => {
    const res = await get("/");
    expect(res.status).toBe(200);
    expect(res.headers.get("content-type")).toBe("text/html; charset=utf-8");
    expect(res.headers.get("x-content-type-options")).toBe("nosniff");
    const csp = res.headers.get("content-security-policy")!;
    for (const d of [
      "default-src 'none'",
      "style-src 'unsafe-inline'",
      "img-src 'self'",
      "font-src 'self'",
      "base-uri 'none'",
      "form-action 'none'",
      "frame-ancestors 'none'",
    ])
      expect(csp).toContain(d);
    const html = await res.text();
    expect(html).toMatch(/^<!doctype html>\n<html lang="en">/);
    expect(html).toContain("<title>Kith</title>");
    expect(html).toContain("A home for the agents you build.");
    expect(html).not.toMatch(/<script[^>]*\ssrc/i);
    const scripts = [...html.matchAll(/<script>([\s\S]*?)<\/script>/g)].map((m) => m[1]);
    expect(scripts).toEqual([LANDING_SCRIPT]);
    const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(scripts[0])));
    expect(csp).toContain(`script-src 'sha256-${btoa(String.fromCharCode(...digest))}'`);
  });

  it("loads nothing from another site: images and fonts are its own", async () => {
    const html = await (await get("/")).text();
    expect(html).not.toMatch(/(?:src=|url\()["']?(?:https?:)?\/\//);
    expect(html).not.toContain("fonts.googleapis.com");
    expect(html).not.toContain("claude:viewport-fit-sides");
    const own = [...html.matchAll(/(?:src="|href="|url\()((?:img|fonts)\/[^")]+)/g)].map((m) => m[1]);
    expect(own.length).toBeGreaterThan(0);
    for (const p of new Set(own)) expect((await asset(`/${p}`)).status, p).toBe(200);
  });

  it("claims nothing the product doesn't do, and answers the six questions", async () => {
    const html = await (await get("/")).text();
    for (const no of ["Linux</a>", "Cursor", "Muse", "0.1.3", "raw.githubusercontent.com"]) expect(html, no).not.toContain(no);
    const asked = [...html.matchAll(/<summary>([^<]+)</g)].map((m) => m[1]);
    expect(asked).toEqual([
      "What do I need?",
      "What does it cost?",
      "What does the person I invite need?",
      "What if my Mac is off?",
      "What leaves my Mac?",
      "Does it run on Linux or Windows?",
    ]);
  });

  it("is one script, real buttons, and a complete page without it", async () => {
    const html = await (await get("/")).text();
    expect(html.match(/<script/g)).toHaveLength(1);
    const still = html.replace(/<script>[\s\S]*?<\/script>/, "");
    expect([...still.matchAll(/<button class="crow" type="button" data-a="(\w+)"/g)].map((m) => m[1])).toEqual([
      "video",
      "coder",
      "designer",
      "reading",
      "finance",
      "mail",
    ]);
    expect([...still.matchAll(/<button class="tbtn" type="button" data-f="([\w.]+)"/g)].map((m) => m[1])).toEqual([
      "agent.md",
      "skills",
      "tools",
      "memory",
      "data",
      "files",
    ]);
    // Without the script: video's chat is open and the folder shows agent.md.
    expect(still).toContain("Post the teaser now, or wait for Friday?");
    expect(still).toContain("role/agent.md · shareable");
    expect(still).toMatch(/harness: <b id="hv">claude<\/b>/);
    for (const engine of ["Claude Code", "Codex", "pi"]) expect(still).toContain(`>${engine}</button>`);
  });

  it("gives every image a size and an alt", async () => {
    const html = (await (await get("/")).text()).replace(/<script>[\s\S]*?<\/script>/, "");
    const sized = [...html.matchAll(/<img [^>]*>/g)].map((m) => m[0]).filter((t) => !t.includes("img/mark.png"));
    expect(sized.length).toBeGreaterThan(5);
    for (const t of sized) expect(t).toMatch(/ width="\d+" height="\d+" alt="/);
  });
});

describe("static assets", () => {
  it("serves images and fonts with their type and a long cache", async () => {
    for (const [path, type] of [
      ["/img/mark.png", "image/png"],
      ["/img/sky-night.jpg", "image/jpeg"],
      ["/fonts/inter-latin.woff2", "font/woff2"],
    ]) {
      const res = await asset(path);
      expect(res.status, path).toBe(200);
      expect(res.headers.get("content-type"), path).toBe(type);
      expect(res.headers.get("cache-control"), path).toBe("public, max-age=2592000");
    }
  });

  it("serves no page or config of its own", async () => {
    for (const p of ["/", "/_headers", "/index.html"]) expect((await asset(p)).status, p).toBe(404);
  });

  it("leaves the download links to the Worker", async () => {
    const res = await get("/download/mac");
    expect(res.status).toBe(302);
    expect(res.headers.get("location")).toBe("https://github.com/Ying-Kai-Liao/kith/releases/latest/download/kith-macos.dmg");
  });
});
