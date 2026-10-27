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
    expect(html).toContain("Your agents. Your computer. Your network.");
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
